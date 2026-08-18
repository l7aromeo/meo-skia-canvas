//
// Neon <-> Node interface
//

"use strict";

const { inspect } = require("util"),
  { loadSkiaNode } = require("../binary");

// if defined, throw TypeErrors for canvas API calls with invalid arguments
const STRICT = !["0", "false", "off"].includes(
  (process.env.SKIA_CANVAS_STRICT || "0").trim().toLowerCase(),
);

const ø = Symbol.for("📦"), // the attr every caller reads the struct through
  // Where the struct actually sits. Not `ø`: an own property has to be
  // defined on every instance, and `Object.defineProperty` is the single
  // most expensive thing about making a wrapped object -- 55 of the 83
  // nanoseconds the JavaScript half adds to a `new Path2D`, against 86 for
  // the Rust call it wraps. A plain store into a private symbol costs
  // nothing measurable, and reading it back through an accessor costs 0.2 ns
  // more than reading an own property did.
  //
  // `ø` is the accessor over it, installed by `handled` below on the
  // prototype rather than the instance. That keeps every property `ø` had:
  // it is not enumerable, it is not an own property so a spread cannot carry
  // a handle out of an object, and assigning to it still throws in strict
  // mode, an accessor with no setter rather than a non-writable value.
  //
  // A class that records its drawing replaces the accessor with one that
  // hands the batch over first -- see `drawlist.guard`. That arrangement was
  // already here for those classes alone; this is the same thing for all of
  // them.
  HANDLE = Symbol("📦.handle"),
  core = (obj) => (obj || {})[ø], // dereference the boxed struct
  /** Reads `ø` on `klass`'s instances straight out of the private slot. */
  handled = (klass) =>
    Object.defineProperty(klass.prototype, ø, {
      get() {
        return this[HANDLE];
      },
      enumerable: false,
      configurable: true,
    }),
  wrap = (type, struct) => {
    // create new instance for struct
    if (!Object.hasOwn(type.prototype, "native"))
      internal(type.prototype, "native", neon[type.name]);
    let obj = Object.create(type.prototype);
    obj[HANDLE] = struct;
    // Still `struct &&`: a wrap of nothing answers with nothing, which is
    // what callers that pass an optional handle through here rely on.
    return struct && obj;
  },
  skiaNode = loadSkiaNode(),
  neon = Object.entries(skiaNode).reduce((api, [name, fn]) => {
    // Match ClassName_methodName or ClassName_get_attr / ClassName_set_attr patterns.
    // Standalone functions (no underscore like "backend") are accessed directly via skiaNode.
    const match = name.match(/^(.+?)_(?:([sg]et)_)?(.+)$/);
    if (!match) return api;
    let [, struct, getset, attr] = match,
      cls = api[struct] || (api[struct] = {}),
      slot = getset ? cls[attr] || (cls[attr] = {}) : cls;
    slot[getset || attr] = fn;
    return api;
  }, {});

// Set by `drawlist` for the classes that record their drawing. Returns true
// when it took the write, false when it has to cross now.
let recordWrite = () => false;
const onRecord = (fn) => {
  recordWrite = fn;
};

class RustClass {
  constructor(type) {
    if (!Object.hasOwn(type.prototype, "native"))
      internal(type.prototype, "native", neon[type.name]);
  }

  alloc(...args) {
    try {
      return this.init("new", ...args);
    } catch (error) {
      rustError(error, this.alloc);
    }
  }

  init(fn, ...args) {
    try {
      this[HANDLE] = this.native[fn](null, ...args);
    } catch (error) {
      rustError(error, this.init);
    }
  }

  ref(key, val) {
    return arguments.length > 1
      ? (this[Symbol.for(key)] = val)
      : this[Symbol.for(key)];
  }

  prop(attr, ...vals) {
    // A write with a declared verb behind it is recorded rather than made.
    // Reads fall through: the answer has to come from Rust, and reading the
    // handle below drains whatever is pending first.
    if (vals.length && recordWrite(this, attr, vals[0])) return;
    try {
      let getset = arguments.length > 1 ? "set" : "get";
      return this.native[attr][getset](this[ø], ...vals);
    } catch (error) {
      rustError(error, this.prop);
    }
  }

  ƒ(fn, ...args) {
    try {
      return this.native[fn](this[ø], ...args);
    } catch (error) {
      rustError(error, this.ƒ);
    }
  }
}

// Once, on the base, so every wrapped class reads its handle through the same
// accessor and nothing depends on when a subclass is first constructed. A
// class that records its drawing defines its own `ø` over this one; that
// shadows rather than races, because `drawlist.install` runs as the class's
// module loads and this is already in place by then.
handled(RustClass);

// shorthands for attaching read-only attributes
const readOnly = (obj, attr, value) =>
  Object.defineProperty(obj, attr, {
    value,
    writable: false,
    enumerable: true,
  });

// Attaches a class's table of Rust functions to its prototype, once. Hidden
// from enumeration, and replaceable only by this package.
const internal = (obj, attr, value) =>
  Object.defineProperty(obj, attr, {
    value,
    writable: false,
    enumerable: false,
    configurable: true,
  });

// convert arguments list to a string of type abbreviations
function signature(args) {
  return args
    .map((v) =>
      Array.isArray(v)
        ? "a"
        : { string: "s", number: "n", object: "o" }[typeof v] || "x",
    )
    .join("");
}

// validate number of args in invocation
const argc = (args, ...expected) => {
  if (expected.includes(args.length) || args.length > Math.max(...expected))
    return;
  let error = new TypeError("not enough arguments");
  Error.captureStackTrace(error, argc);
  throw error;
};

// The mark Rust puts on a message it only wants raised in strict mode. Two
// code units, and it used to be cut with `substr(1)` -- which took the warning
// sign and left the variation selector behind, so every strict-mode message
// began with an invisible character that a recorded verb, raising the same
// text from JavaScript, did not have.
const STRICT_ONLY = "⚠️";

// remove internals from stack trace and filter non-strict errors
const rustError = (error, stack) => {
  if (error.message.startsWith(STRICT_ONLY)) {
    if (STRICT) error.message = error.message.slice(STRICT_ONLY.length);
    else return;
  }
  Error.captureStackTrace(error, stack);
  throw error;
};

module.exports = {
  neon,
  ø,
  HANDLE,
  handled,
  STRICT,
  onRecord,
  skiaNode,
  core,
  wrap,
  signature,
  argc,
  readOnly,
  RustClass,
  inspect,
  REPR: inspect.custom,
};
