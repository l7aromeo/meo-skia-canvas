//
// Recording drawing calls instead of making them.
//
// A verb call costs about 70 nanoseconds to cross into Rust, of which roughly
// 17 is the crossing itself and the rest is reading the arguments and unboxing
// the receiver. The work behind most verbs is smaller than that -- appending a
// line segment to a path is a few nanoseconds -- so the boundary, not the
// drawing, is what a path-heavy frame spends its time on.
//
// So the calls are written into a buffer and handed over in one crossing when
// something actually needs an answer. Measured with the decoder this feeds:
// a hundred thousand `lineTo` calls take 6.79 ms one at a time and 1.40 ms as
// one batch, and a frame-shaped mix of property writes and drawing verbs 1.78
// ms against 1.00.
//
// The verbs come from Rust. `Path2D_verbTable()` reports every verb, its
// opcode, its arguments and their rules, and the writers below are generated
// from that -- so the opcode a writer emits and the opcode the decoder reads
// are the same number by construction rather than by two lists agreeing.
//

"use strict";

const { neon, STRICT, ø, RAW, core, onRecord } = require("./neon");

// One buffer for the process, not one per object. A drawing builds thousands
// of short-lived paths, and giving each its own kilobytes to record four verbs
// into would cost more memory than the crossings save. Only one object records
// at a time; another starting to record flushes the first.
const INITIAL_SLOTS = 1024;
let arena = new Float64Array(INITIAL_SLOTS);
let used = 0;
// What a buffer of numbers cannot hold. A record stores an index into this
// instead of the value: a colour, a font, an enum name. Cleared with the
// arena, or it would keep alive whatever the caller has since dropped.
let values = [];
let owner = null; // the object whose verbs are in the arena
let commit = null; // how to hand them over

/** Hands the recorded verbs to Rust and empties the arena. */
function flush() {
  if (!used) {
    owner = null;
    return;
  }
  const [target, plot, length, slots] = [owner, commit, used, values];
  // Cleared first: `plot` reaches back into JavaScript for the handle, and a
  // half-emptied arena reachable from there would be replayed twice.
  used = 0;
  owner = null;
  commit = null;
  values = [];
  plot(target, arena, length, slots);
}

/** Room for `slots` more numbers, flushing another object's work first. */
function reserve(target, plot, slots) {
  if (owner !== target) {
    flush();
    owner = target;
    commit = plot;
  }
  if (used + slots > arena.length) {
    if (used) {
      // Hand over what is there rather than growing without bound; a batch is
      // as good in two halves as in one.
      flush();
      owner = target;
      commit = plot;
    }
    while (arena.length < slots) arena = new Float64Array(arena.length * 2);
  }
  const at = used;
  used += slots;
  return at;
}

//
// -- what a bad argument does ------------------------------------------------
//

/** `value` as a number, the way this binding has always read one. */
function asNumber(value) {
  // `+value` throws for a symbol and a BigInt, where this binding ignores the
  // call instead. Reproduced rather than corrected: a browser throws for both,
  // and `tests/suite/arguments.test.js` pins today's answer so that changing
  // it is a deliberate commit rather than a side effect of this one.
  const kind = typeof value;
  if (kind === "symbol" || kind === "bigint") return NaN;
  return +value;
}

const ordinals = ["1st", "2nd", "3rd"];
const ordinal = (index) => ordinals[index] || `${index + 1}th`;

/** Refuses a call with fewer arguments than the verb reads. */
function checkArity(names, given) {
  if (given >= names.length) return;
  throw new TypeError(
    `not enough arguments (missing: ${names.slice(given).join(", ")})`,
  );
}

// A rule an argument can carry, named by the schema. One check and one message
// each, shared by every verb that names it, so a verb added later writes no
// new error text.
const RULES = {
  non_negative: {
    fails: (value) => value < 0,
    refuse() {
      throw new RangeError("Radius value must be positive");
    },
  },
};

//
// -- generated writers -------------------------------------------------------
//

/**
 * Gives `klass` a recording method for every verb in `table`.
 *
 * `plot` hands a finished batch to Rust for one of these receivers.
 */
// Property name to the writer that records a write to it, per class.
const setters = new WeakMap();

/**
 * Records a property write, or answers false so the caller crosses now.
 *
 * Reached from `RustClass.prop`, so the class's own setter has already run --
 * `imageSmoothingEnabled` coerces its argument to a boolean there, and losing
 * that by replacing the property outright would change what `= 1` means.
 */
onRecord((target, attr, value) => {
  const write = setters.get(target.constructor)?.[attr];
  if (!write) return false;
  // A writer may decline -- a property whose value is a handle this batch
  // cannot carry -- in which case the caller crosses as before.
  return write.call(target, value) !== false;
});

/** The generated writers for `klass`, by verb name. */
const written = new WeakMap();

/** The writer for one verb, for a wrapper that chooses between several. */
const writerFor = (klass, verb) => written.get(klass)?.[verb];

function install(klass, table, plot) {
  guard(klass);
  const writes = {};
  const verbs = {};
  setters.set(klass, writes);
  written.set(klass, verbs);
  for (const [verb, spec] of Object.entries(table)) {
    const names = spec.args.map((arg) => arg.name);
    const rules = spec.args.map((arg) => RULES[arg.kind] || null);
    const texts = spec.args.map((arg) => arg.kind === "text");
    const handles = spec.args.map((arg) => arg.kind === "handle");
    const { op, arity, flag } = spec;
    const count = names.length;

    const written_ = function () {
      checkArity(names, arguments.length);

      const at = reserve(this, plot, arity + 1);
      arena[at] = op;
      for (let i = 0; i < count; i++) {
        if (texts[i]) {
          // Not a number and never was: the record holds where the value sits
          // in the array travelling beside the buffer.
          arena[at + 1 + i] = values.push(arguments[i]) - 1;
          continue;
        }
        if (handles[i]) {
          // The boxed struct, not the wrapper around it -- and taking it
          // drains whatever that object had recorded, so the path captured
          // here is the path as it stands at the call.
          arena[at + 1 + i] = values.push(core(arguments[i])) - 1;
          continue;
        }
        const value = asNumber(arguments[i]);
        const rule = rules[i];
        if (rule && rule.fails(value)) {
          // Nothing was written that a later flush would replay: the slots
          // are reserved, so back them out before refusing.
          used -= arity + 1;
          rule.refuse();
        }
        if (STRICT && !isFinite(value)) {
          used -= arity + 1;
          throw new TypeError(
            `⚠️Expected a number for \`${names[i]}\` as ${ordinal(i)} arg`,
          );
        }
        arena[at + 1 + i] = value;
      }
      // A boolean, and only a boolean: `bool_arg_or` reads nothing else, so
      // `arc(x, y, r, a, b, 1)` sweeps clockwise here as it does on a call.
      if (flag) arena[at + 1 + count] = arguments[count] === true ? 1 : 0;
    };

    verbs[verb] = written_;
    if (verb.startsWith("set_")) {
      // Taken by `prop`, not installed as a method: the property already
      // exists on the class with a getter beside it.
      const property = verb.slice(4);
      if (property.endsWith("Text")) {
        // A verb declared for the string form of a property that also accepts
        // an object -- `fillStyle` takes a gradient too, and a handle has
        // nowhere to go in a batch. Record a string; let anything else cross
        // the way it always did.
        const wanted = property.slice(0, -4);
        writes[wanted] = function (value) {
          if (typeof value !== "string") return false;
          written_.call(this, value);
        };
      } else {
        writes[property] = written_;
      }
    } else {
      Object.defineProperty(klass.prototype, verb, {
        value: written_,
        writable: true,
        enumerable: false,
        configurable: true,
      });
    }
  }
}

/** The boxed struct behind `target`, with no flush. Only `flush` may use it. */
const rawHandle = (target) => target[RAW];

/**
 * Makes reading `klass`'s boxed handle drain the arena first.
 *
 * The flush is the whole safety of this: Rust must never see an object with
 * verbs still pending. Every accessor calling `flush()` first is the usual way
 * to arrange that, and it fails the first time someone adds an accessor and
 * forgets. Reading the handle *is* the boundary, so the drain happens there
 * and "forgot to flush" stops being expressible.
 *
 * On the prototype, not on each object: defining a property per instance
 * changes its shape, which measured at 886 nanoseconds to build a `Path2D`
 * against 405 -- more than the recording saves on a short path.
 */
function guard(klass) {
  Object.defineProperty(klass.prototype, ø, {
    get() {
      if (owner === this) flush();
      return this[RAW];
    },
    enumerable: false,
    configurable: true,
  });
  klass.records = true;
}

module.exports = { install, guard, flush, rawHandle, writerFor, neon };
