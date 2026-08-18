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

const ø = Symbol.for("📦"), // the attr containing the boxed struct
  RAW = Symbol("📦.raw"), // where a recording class keeps it instead
  // A class that records its drawing keeps the handle out of `ø` and puts a
  // draining accessor there instead -- on the prototype, so an instance costs
  // nothing to make. See `drawlist.install`.
  slot = (type) => (type.records ? RAW : ø),
  core = (obj) => (obj || {})[ø], // dereference the boxed struct
  wrap = (type, struct) => {
    // create new instance for struct
    let obj = internal(Object.create(type.prototype), slot(type), struct);
    return struct && internal(obj, "native", neon[type.name]);
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
    internal(this, "native", neon[type.name]);
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
      return internal(
        this,
        slot(this.constructor),
        this.native[fn](null, ...args),
      );
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

// shorthands for attaching read-only attributes
const readOnly = (obj, attr, value) =>
  Object.defineProperty(obj, attr, {
    value,
    writable: false,
    enumerable: true,
  });

const internal = (obj, attr, value) =>
  Object.defineProperty(obj, attr, {
    value,
    writable: false,
    enumerable: false,
    // Replaceable, so a class that records its drawing can put a draining
    // accessor here instead -- see `drawlist.guard`. Still not writable and
    // still hidden from enumeration; only redefinition is allowed, and only
    // this package can reach the symbol to attempt it.
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

// remove internals from stack trace and filter non-strict errors
const rustError = (error, stack) => {
  if (error.message.startsWith("⚠️")) {
    if (STRICT) error.message = error.message.substr(1);
    else return;
  }
  Error.captureStackTrace(error, stack);
  throw error;
};

module.exports = {
  neon,
  ø,
  RAW,
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
