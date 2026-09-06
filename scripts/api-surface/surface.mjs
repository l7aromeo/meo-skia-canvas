//
// The two surfaces this compares, built independently so they can disagree.
//
// `lib/index.d.ts` is a claim about the runtime, and a claim is only worth
// gating if something checks it. A member the runtime has and the file does
// not declare is invisible to every TypeScript caller; a member declared and
// absent is a lie to them. Both directions have shipped here.
//
import { createRequire } from "module";
import { readFileSync } from "fs";

const require = createRequire(import.meta.url);
const ts = require("typescript");

// Statics every function carries, and the one prototype link that is not a
// member of anything.
const INTRINSIC_STATIC = new Set(["length", "name", "prototype"]);

/**
 * What the built addon actually exposes, per exported class or object.
 *
 * Walks the prototype chain rather than one prototype: the wrapper puts its
 * verbs on a base class, and a caller reaches them through the subclass. Own
 * instance properties are read where a value can be built, because the
 * binding installs some state on the instance and a prototype-only walk
 * cannot see it.
 */
export const runtimeSurface = (mod, samples = {}) => {
  const surface = new Map();
  const add = (holder, key) => {
    if (typeof key !== "string") return; // symbols are not declarable here
    if (INTRINSIC_STATIC.has(key)) return;
    // A leading underscore is this ecosystem's "not yours" -- Node's own
    // EventEmitter carries `_events`, `_eventsCount` and `_maxListeners` on
    // every instance, and no declaration file anywhere declares them.
    if (key.startsWith("_")) return;
    if (!surface.has(holder)) surface.set(holder, new Set());
    surface.get(holder).add(key);
  };
  const walkChain = (start, holder, skipConstructor) => {
    let proto = start;
    while (proto && proto !== Object.prototype) {
      for (const key of Reflect.ownKeys(proto))
        if (!(skipConstructor && key === "constructor")) add(holder, key);
      proto = Object.getPrototypeOf(proto);
    }
  };

  for (const [name, value] of Object.entries(mod)) {
    const type = typeof value;
    if (type !== "function" && (type !== "object" || value === null)) continue;
    surface.set(name, surface.get(name) ?? new Set());

    if (type === "function") {
      for (const key of Reflect.ownKeys(value)) add(name, key);
      walkChain(value.prototype, name, true);
    } else {
      // An exported instance -- `App` and `FontLibrary` are objects, not
      // classes, so a sweep written against `.prototype` misses them.
      walkChain(Object.getPrototypeOf(value), name, true);
      for (const key of Reflect.ownKeys(value)) add(name, key);
    }
  }

  for (const [name, make] of Object.entries(samples)) {
    let instance;
    try {
      instance = make();
    } catch {
      continue; // reported by the caller if it matters; not a surface claim
    }
    for (const key of Reflect.ownKeys(instance)) add(name, key);
  }
  return surface;
};

/**
 * What the declarations claim, per class or interface, with `extends` closed
 * over.
 *
 * The heritage closure is not optional. These declarations follow WebIDL and
 * split the context across `CanvasPath`, `CanvasDrawPath` and a dozen more
 * mixin interfaces, so a member is usually declared on an interface whose
 * name no caller writes. Without closing over `extends`, `Path2D.moveTo`
 * reads as undeclared.
 *
 * `externals` are declaration files the surface inherits from but does not
 * contain -- `Image` and `Window` extend `EventEmitter` from "stream", and
 * without @types/node the whole emitter surface reports as undeclared.
 */
export const declaredSurface = (entry, externals = []) => {
  const members = new Map();
  const heritage = new Map();
  let source;

  // A computed name -- `[EventEmitter.captureRejectionSymbol]` -- is a
  // symbol, and a symbol is not a member a caller writes by name. Skipped on
  // this side because the runtime side skips symbols too, so counting it here
  // would report a difference that is only in the instrument.
  const named = (node) => {
    if (node.name && ts.isComputedPropertyName(node.name)) return undefined;
    return node.name?.getText(source).replace(/^["'`]|["'`]$/g, "");
  };
  const record = (holder, list) => {
    if (!members.has(holder)) members.set(holder, new Set());
    for (const member of list) {
      const key = named(member);
      if (key && !INTRINSIC_STATIC.has(key)) members.get(holder).add(key);
    }
  };

  const visit = (node) => {
    if (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) {
      const name = named(node);
      if (name) {
        record(name, node.members);
        heritage.set(name, [
          ...(heritage.get(name) ?? []),
          ...(node.heritageClauses ?? []).flatMap((clause) =>
            clause.types.map((t) => t.expression.getText(source)),
          ),
        ]);
      }
    } else if (ts.isFunctionDeclaration(node)) {
      // Declared with no members of its own. Recorded so that a module-level
      // function counts as declared rather than as an export nothing claims.
      const name = named(node);
      if (name && !members.has(name)) members.set(name, new Set());
    } else if (ts.isVariableStatement(node)) {
      // `export const TextDecoration: { readonly Underline: 0x1; ... }` is a
      // namespace of members, and walking only the name reports every one of
      // them as undeclared.
      for (const declaration of node.declarationList.declarations) {
        const name = declaration.name.getText(source);
        if (!members.has(name)) members.set(name, new Set());
        if (declaration.type && ts.isTypeLiteralNode(declaration.type))
          record(name, declaration.type.members);
      }
    }
    ts.forEachChild(node, visit);
  };

  for (const file of [entry, ...externals]) {
    source = ts.createSourceFile(
      file,
      readFileSync(file, "utf8"),
      ts.ScriptTarget.Latest,
      true,
    );
    visit(source);
  }

  // Declaration merging: a `type` and a `const` of one name are two entities
  // to the compiler and one to a caller, so the members of both are the
  // caller's surface. `members` is keyed by name, so they have already
  // merged -- this is the note that says that is deliberate.
  const closed = (name, seen = new Set()) => {
    if (seen.has(name)) return new Set();
    seen.add(name);
    const out = new Set(members.get(name) ?? []);
    for (const base of heritage.get(name) ?? [])
      for (const key of closed(base, seen)) out.add(key);
    return out;
  };

  return { has: (name) => members.has(name), closed };
};
