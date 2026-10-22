//
// The parameters of every method and function on both surfaces, per
// overload, for the parameter pass of the parity gate.
//
// The item extractors answer whether a name exists on each side; two methods
// pairing by name says nothing about whether they take the same things. A
// `loadImage(src: string | URL | Buffer)` against a `from_encoded(bytes:
// &[u8])` pairs as well as two identical signatures do. This records, for
// each parameter, its name, the coarse classes of value it accepts and
// whether it may be left out, so that a JavaScript parameter can be held to
// the Rust function it pairs with.
//
// Kept apart from the item extractors on purpose: they define the
// interchange contract the gate pairs on, and parameters are a second
// question asked of the same declarations. Neither extractor changes.
//
// CLASSES are deliberately coarse -- `number`, `string`, `boolean`, `enum`,
// `bytes`, `path`, `array`, `object`, `callback` -- because the two type
// systems agree on little finer. A union keeps every class it can hold, so
// `string | URL | Buffer` is three classes and a Rust `&[u8]` covers one of
// them. `unknown` is a type this cannot classify, and is reported rather
// than guessed. `range` is a Rust `Range`, which JavaScript passes as two
// numbers.
//
// BEYOND THE CLASSES, three things the two surfaces spell differently: a
// Rust struct with only public fields lists them, since JavaScript passes
// such a struct as its fields; a Rust method records that it takes `self`;
// and an npm member records whether it is static, since a static helper
// passes as its first argument what the Rust method takes as `self`.

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { surfaceOf } from "../rustdoc-surface.mjs";
import { ts } from "../api-surface/typescript.mjs";

// ---------------------------------------------------------------- Rust

/**
 * A struct as a parameter: an `object`, and also what it can be spelled as.
 *
 * `fields` lists its public fields, one level deep, when every field is
 * public -- JavaScript passes such a struct as its fields, either spread into
 * positional arguments (`setTransform(a, b, c, d, e, f)` for an `Affine`) or
 * as an options object whose keys are the field names. A struct with a
 * private field has no such spelling and gets no list.
 *
 * A struct with an inherent `from_css(&str)` takes a string too, since that
 * is how the crate reads the CSS spelling JavaScript passes: a CSS transform
 * for an `Affine`.
 */
function structType(target, index, nested) {
  const plain = target?.inner?.struct?.kind?.plain;
  const fieldItems = (plain?.fields ?? []).map((id) => index[String(id)]);
  const open =
    plain != null &&
    !plain.has_stripped_fields &&
    fieldItems.length > 0 &&
    fieldItems.every((f) => f?.visibility === "public");
  const parses = (target?.inner?.struct?.impls ?? []).some((id) => {
    const impl = index[String(id)]?.inner?.impl;
    if (impl == null || impl.trait != null) return false;
    return impl.items.some((m) => {
      const fn = index[String(m)];
      const inputs = fn?.inner?.function?.sig?.inputs ?? [];
      return (
        fn?.name === "from_css" &&
        inputs.length === 1 &&
        inputs[0][1]?.borrowed_ref?.type?.primitive === "str"
      );
    });
  });
  return {
    classes: parses ? ["object", "string"] : ["object"],
    optional: false,
    ...(open && !nested
      ? {
          fields: fieldItems.map((f) => ({
            name: f.name,
            ...rustType(f.inner.struct_field, index, true),
          })),
        }
      : {}),
  };
}

/**
 * The classes a rustdoc type accepts, and whether it is an `Option`.
 *
 * `nested` is set for a struct's own fields, which are classified but not
 * expanded further: JavaScript spreads one level of a struct, never two.
 */
function rustType(type, index, nested = false) {
  if (type == null) return { classes: ["unknown"], optional: false };
  const [kind] = Object.keys(type);
  const body = type[kind];
  const one = (c) => ({ classes: [c], optional: false });
  switch (kind) {
    case "primitive":
      if (body === "bool") return one("boolean");
      if (body === "str" || body === "char") return one("string");
      return one("number");
    case "borrowed_ref":
      return rustType(body.type, index, nested);
    // A run of `u8` is bytes as well as a list: `[u8; 256]` is a lookup
    // table JavaScript passes as a `Uint8Array` or a `number[]`.
    case "slice":
      return {
        classes: body?.primitive === "u8" ? ["bytes", "array"] : ["array"],
        optional: false,
      };
    case "array":
      return {
        classes:
          body?.type?.primitive === "u8" ? ["bytes", "array"] : ["array"],
        optional: false,
      };
    case "tuple":
      return one("array");
    case "function_pointer":
      return one("callback");
    case "generic":
      return one("unknown");
    case "impl_trait": {
      const bound = body.find((b) => b.trait_bound)?.trait_bound?.trait;
      const name = bound?.path?.split("::").pop() ?? "";
      const arg = bound?.args?.angle_bracketed?.args?.[0]?.type;
      if (/^Fn(Mut|Once)?$/.test(name)) return one("callback");
      if (name === "AsRef" || name === "Into") {
        const target = arg ? rustType(arg, index) : one("unknown");
        if (arg?.resolved_path?.path?.endsWith("Path")) return one("path");
        return target;
      }
      if (name === "IntoIterator") return one("array");
      return one("unknown");
    }
    case "resolved_path": {
      const name = body.path.split("::").pop();
      const arg = body.args?.angle_bracketed?.args?.[0]?.type;
      if (name === "Option") {
        const inner = rustType(arg, index, nested);
        return { ...inner, optional: true };
      }
      if (name === "Box" || name === "Rc" || name === "Arc")
        return rustType(arg, index, nested);
      if (name === "String") return one("string");
      if (name === "Path" || name === "PathBuf") return one("path");
      if (name === "Vec") {
        return {
          classes: arg?.primitive === "u8" ? ["bytes", "array"] : ["array"],
          optional: false,
        };
      }
      // Two numbers in JavaScript, `start` and `end`, are one `Range` here.
      if (name === "Range") return one("range");
      const target = index[String(body.id)];
      const targetKind = Object.keys(target?.inner ?? {})[0];
      if (targetKind === "enum") return one("enum");
      return structType(target, index, nested);
    }
    case "dyn_trait":
      return one("callback");
    default:
      return one("unknown");
  }
}

/** One function's record: its receiver, if any, and its parameters. */
function functionRecord(node, index) {
  const all = node.inner.function.sig.inputs;
  const inputs = all.filter(([name]) => name !== "self");
  return {
    // A method's receiver, which a JavaScript static helper passes as its
    // first argument: `ColorMatrix.postTranslate(m, dx, dy)` for
    // `ColorMatrix::post_translate(&mut self, dx, dy)`.
    receiver: inputs.length < all.length,
    params: inputs.map(([name, type]) => ({
      name,
      ...rustType(type, index),
    })),
  };
}

/**
 * `{ "Owner::name": { params: [...] } }` for every reachable Rust function
 * that the item extractor would emit as a method or function -- inherent
 * methods and free functions, never trait-impl methods -- under the same id.
 */
export function rustParams(doc) {
  const { item, reachable, owner, paths } = surfaceOf(doc);
  const kindOf = (node) => Object.keys(node?.inner ?? {})[0];
  const namedAncestor = (id) => {
    for (let at = owner.get(id); at !== undefined; at = owner.get(at)) {
      const path = paths[String(at)]?.path;
      if (path) return path[path.length - 1];
    }
    return null;
  };
  const inTraitImpl = (id) => {
    const parent = item(owner.get(id));
    return kindOf(parent) === "impl" && parent.inner.impl.trait != null;
  };
  const out = {};
  for (const id of reachable) {
    const node = item(id);
    if (kindOf(node) !== "function" || !node.name || inTraitImpl(id)) continue;
    const on = namedAncestor(id);
    const key = on ? `${on}::${node.name}` : node.name;
    if (out[key]) continue;
    out[key] = functionRecord(node, doc.index);
  }
  return out;
}

// ---------------------------------------------------------------- npm

const BYTES = new Set([
  "Buffer",
  "ArrayBuffer",
  "SharedArrayBuffer",
  "Uint8Array",
  "Uint8ClampedArray",
]);
const NUMERIC_ARRAYS = new Set([
  "Int8Array",
  "Int16Array",
  "Uint16Array",
  "Int32Array",
  "Uint32Array",
  "Float32Array",
  "Float64Array",
]);

/** The classes a TypeScript type accepts. */
function npmClasses(type, checker, seen = new Set()) {
  if (seen.has(type)) return ["object"];
  seen.add(type);
  const F = ts.TypeFlags;
  if (type.isUnion()) {
    const all = type.types.flatMap((t) => npmClasses(t, checker, seen));
    // A union of string literals is an enumeration; a literal beside a plain
    // string is still a string.
    const out = new Set(all.filter((c) => c !== "none"));
    if (out.has("string")) out.delete("enum");
    return [...out];
  }
  if (type.flags & (F.Undefined | F.Null | F.Void)) return ["none"];
  if (type.flags & F.StringLiteral) return ["enum"];
  if (type.flags & (F.String | F.TemplateLiteral)) return ["string"];
  if (type.flags & (F.Number | F.NumberLiteral | F.BigInt)) return ["number"];
  if (type.flags & (F.Boolean | F.BooleanLiteral)) return ["boolean"];
  if (type.flags & F.Any || type.flags & F.Unknown) return ["unknown"];
  const name = type.getSymbol()?.getName() ?? type.aliasSymbol?.getName();
  if (BYTES.has(name)) return ["bytes"];
  if (NUMERIC_ARRAYS.has(name)) return ["array"];
  if (name === "URL") return ["path"];
  if (checker.isArrayType(type) || checker.isTupleType(type)) return ["array"];
  if (["Iterable", "ArrayLike", "ReadonlyArray"].includes(name))
    return ["array"];
  if (type.getCallSignatures().length > 0) return ["callback"];
  return ["object"];
}

/**
 * `{ "Holder.member": { overloads: [{ params: [...] }] } }` for every method,
 * constructor (as `Holder.new`) and top-level function in `entry`, keyed as
 * the npm item extractor keys them: by the holder that declares the member.
 */
export function npmParams(entry) {
  const program = ts.createProgram([entry], {
    noEmit: true,
    skipLibCheck: true,
  });
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(entry);
  const out = {};
  const add = (key, decl, isStatic = false) => {
    const params = decl.parameters.map((p) => {
      const type = checker.getTypeAtLocation(p);
      const classes = npmClasses(type, checker);
      return {
        name: p.name.getText(source),
        classes: classes.filter((c) => c !== "none"),
        optional: Boolean(
          p.questionToken ||
          p.initializer ||
          p.dotDotDotToken ||
          classes.includes("none"),
        ),
        rest: Boolean(p.dotDotDotToken),
      };
    });
    (out[key] ??= { overloads: [] }).overloads.push({
      params,
      static: isStatic,
    });
  };
  const visit = (node) => {
    if (
      (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) &&
      node.name
    ) {
      const holder = node.name.text;
      for (const m of node.members) {
        if (
          ts.isConstructorDeclaration(m) ||
          ts.isConstructSignatureDeclaration(m)
        ) {
          add(`${holder}.new`, m);
        } else if (
          (ts.isMethodDeclaration(m) || ts.isMethodSignature(m)) &&
          m.name
        ) {
          const flags = ts.getCombinedModifierFlags(m);
          add(
            `${holder}.${m.name.getText(source)}`,
            m,
            (flags & ts.ModifierFlags.Static) !== 0,
          );
        }
      }
    } else if (ts.isFunctionDeclaration(node) && node.name) {
      add(node.name.text, node);
    } else if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.type &&
      ts.isTypeLiteralNode(node.type)
    ) {
      // `declare var DOMMatrix: { new (...): DOMMatrix; fromFloat32Array(...) }`
      // and `export const ColorMatrix: { concat(...) }`: the DOM's way of
      // declaring a constructor and its statics, and a namespace of helpers.
      const holder = node.name.text;
      for (const m of node.type.members) {
        if (ts.isConstructSignatureDeclaration(m)) add(`${holder}.new`, m);
        else if (ts.isMethodSignature(m) && m.name) {
          add(`${holder}.${m.name.getText(source)}`, m, true);
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return out;
}

// ---------------------------------------------------------------- self-test

/**
 * The classifier on the shapes it has to tell apart, each built the way
 * rustdoc and the TypeScript checker present them -- a classifier that
 * folded bytes into lists, or an `Option` into its payload without marking
 * it, would pass every pairing it was wrong about.
 */
function selfTest() {
  const prim = (name) => ({ primitive: name });
  const path = (name, args = [], id = 0) => ({
    resolved_path: {
      path: name,
      id,
      args: { angle_bracketed: { args: args.map((type) => ({ type })) } },
    },
  });
  const asRefPath = {
    impl_trait: [
      {
        trait_bound: {
          trait: {
            path: "AsRef",
            args: { angle_bracketed: { args: [{ type: path("Path") }] } },
          },
        },
      },
    ],
  };
  const field = (name, type, visibility = "public") => ({
    name,
    visibility,
    inner: { struct_field: type },
  });
  const struct = (fields, impls = []) => ({
    inner: { struct: { kind: { plain: { fields } }, impls } },
  });
  const fromCss = {
    name: "from_css",
    inner: {
      function: {
        sig: {
          inputs: [["css", { borrowed_ref: { type: prim("str") } }]],
        },
      },
    },
  };
  const index = {
    1: { inner: { enum: {} } },
    2: { inner: { struct: {} } },
    3: struct([10, 11]),
    4: struct([10, 12]),
    5: struct([10], [20]),
    6: struct([13]),
    10: field("x", prim("f32")),
    11: field("y", prim("f32")),
    12: field("hidden", prim("f32"), "default"),
    13: field("origin", path("Point", [], 3)),
    20: { inner: { impl: { trait: null, items: [21] } } },
    21: fromCss,
  };
  const rustCases = [
    ["f32", prim("f32"), "number", false],
    ["bool", prim("bool"), "boolean", false],
    ["&str", { borrowed_ref: { type: prim("str") } }, "string", false],
    ["String", path("String"), "string", false],
    [
      "&[u8]",
      { borrowed_ref: { type: { slice: prim("u8") } } },
      "bytes,array",
      false,
    ],
    [
      "[u8; 256]",
      { array: { type: prim("u8"), len: "256" } },
      "bytes,array",
      false,
    ],
    ["[f32; 4]", { array: { type: prim("f32"), len: "4" } }, "array", false],
    ["Vec<u8>", path("Vec", [prim("u8")]), "bytes,array", false],
    ["Vec<f32>", path("Vec", [prim("f32")]), "array", false],
    ["Option<f32>", path("Option", [prim("f32")]), "number", true],
    ["Range<usize>", path("Range", [prim("usize")]), "range", false],
    ["PathBuf", path("PathBuf"), "path", false],
    ["impl AsRef<Path>", asRefPath, "path", false],
    ["an enum", path("BlendMode", [], 1), "enum", false],
    ["a struct", path("Rect", [], 2), "object", false],
    ["a struct with a from_css", path("Affine", [], 5), "object,string", false],
  ];
  let bad = 0;
  for (const [label, type, classes, optional] of rustCases) {
    const got = rustType(type, index);
    if (got.classes.join() !== classes || got.optional !== optional) {
      console.error(
        `  self-test FAILED: Rust ${label} read as ${JSON.stringify(got)}`,
      );
      bad += 1;
    }
  }
  // What a struct can be spelled as: its public fields one level down, and
  // nothing for a struct with a private one.
  const fieldsOf = (id) =>
    (rustType(path("S", [], id), index).fields ?? [])
      .map((f) => `${f.name}:${f.classes.join("|")}${f.fields ? "+" : ""}`)
      .join(" ");
  const receiverOf = (inputs) =>
    functionRecord({ inner: { function: { sig: { inputs } } } }, index);
  const shapeCases = [
    ["a struct of public fields", fieldsOf(3), "x:number y:number"],
    ["a struct with a private field", fieldsOf(4), ""],
    [
      "a field that is itself a struct, unexpanded",
      fieldsOf(6),
      "origin:object",
    ],
    [
      "a method's receiver",
      String(
        receiverOf([
          ["self", { generic: "Self" }],
          ["dx", prim("f32")],
        ]).receiver,
      ),
      "true",
    ],
    [
      "a receiver is not a parameter",
      receiverOf([
        ["self", { generic: "Self" }],
        ["dx", prim("f32")],
      ])
        .params.map((q) => q.name)
        .join(),
      "dx",
    ],
    [
      "an associated function has no receiver",
      String(receiverOf([["dx", prim("f32")]]).receiver),
      "false",
    ],
  ];
  for (const [label, got, want] of shapeCases) {
    if (got !== want) {
      console.error(
        `  self-test FAILED: Rust ${label} read as ${JSON.stringify(got)}, expected ${JSON.stringify(want)}`,
      );
      bad += 1;
    }
  }
  const dir = mkdtempSync(join(tmpdir(), "parity-params-"));
  const dts = join(dir, "case.d.ts");
  writeFileSync(
    dts,
    [
      "declare class Buffer {}",
      "declare class URL {}",
      "export class Case {",
      "  constructor(width: number, height?: number);",
      "  load(src: string | URL | Buffer): void;",
      "  rule(r: 'nonzero' | 'evenodd'): void;",
      "  table(t: Uint8Array | number[]): void;",
      "  each(f: (x: number) => void): void;",
      "  maybe(x?: number): void;",
      "  static make(width: number): Case;",
      "}",
      "declare var Point: { new (x: number, y: number): object; fromPoint(p: object): object };",
      "export function loose(a: string, ...rest: number[]): void;",
    ].join("\n"),
  );
  const npm = npmParams(dts);
  rmSync(dir, { recursive: true });
  const param = (key, i) => npm[key]?.overloads[0]?.params[i];
  const npmCases = [
    ["a constructor", param("Case.new", 0)?.classes.join(), "number"],
    [
      "an optional constructor argument",
      String(param("Case.new", 1)?.optional),
      "true",
    ],
    [
      "a string, a URL and a Buffer",
      param("Case.load", 0)?.classes.join(),
      "string,path,bytes",
    ],
    [
      "a union of string literals",
      param("Case.rule", 0)?.classes.join(),
      "enum",
    ],
    [
      "a Uint8Array or a number array",
      param("Case.table", 0)?.classes.join(),
      "bytes,array",
    ],
    ["a function", param("Case.each", 0)?.classes.join(), "callback"],
    [
      "a question-marked argument",
      String(param("Case.maybe", 0)?.optional),
      "true",
    ],
    [
      "a constructor declared on a var",
      param("Point.new", 1)?.classes.join(),
      "number",
    ],
    [
      "a static declared on a var",
      param("Point.fromPoint", 0)?.classes.join(),
      "object",
    ],
    ["a static method", String(npm["Case.make"]?.overloads[0]?.static), "true"],
    [
      "an instance method",
      String(npm["Case.load"]?.overloads[0]?.static),
      "false",
    ],
    [
      "a member of a var's type literal",
      String(npm["Point.fromPoint"]?.overloads[0]?.static),
      "true",
    ],
    [
      "a top-level function's rest argument",
      String(param("loose", 1)?.rest),
      "true",
    ],
  ];
  for (const [label, got, want] of npmCases) {
    if (got !== want) {
      console.error(
        `  self-test FAILED: npm ${label} read as ${got}, expected ${want}`,
      );
      bad += 1;
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `parameters: self-test, ${rustCases.length} Rust types, ` +
      `${shapeCases.length} Rust shapes and ${npmCases.length} npm declarations`,
  );
}

// ---------------------------------------------------------------- CLI

const isMain = import.meta.url === `file://${process.argv[1]}`;
if (isMain && process.argv.includes("--self-test")) {
  selfTest();
} else if (isMain) {
  const [, , rustdoc, dts, outPath = "target/parity-params.json"] =
    process.argv;
  const rust = rustParams(JSON.parse(readFileSync(rustdoc, "utf8")));
  const npm = npmParams(dts);
  writeFileSync(outPath, JSON.stringify({ rust, npm }, null, 1) + "\n");
  console.log(
    `parameters: ${Object.keys(rust).length} Rust functions, ` +
      `${Object.keys(npm).length} npm methods -> ${outPath}`,
  );
}
