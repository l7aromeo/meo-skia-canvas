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

const { STRICT, ø, HANDLE, onRecord, RustClass } = require("./neon");

// One buffer for the process, not one per object. A drawing builds thousands
// of short-lived paths, and giving each its own kilobytes to record four verbs
// into would cost more memory than the crossings save. Only one object records
// at a time; another starting to record flushes the first.
// Fixed, and flushed when a record will not fit. Growth would replace the
// array, and the generated writers below hold it directly -- reading it
// through a binding on every store measured slower than the batching saved.
const SLOTS = 8192;
const arena = new Float64Array(SLOTS);
let used = 0;
// What a buffer of numbers cannot hold. A record stores an index into this
// instead of the value: a colour, a font, an enum name. Cleared with the
// arena, or it would keep alive whatever the caller has since dropped.
let values = [];
// The objects those values point at, for as long as they are pointed at.
//
// A record cannot hold a path, so it holds the boxed handle of the `Path2D`
// and the path is read out when the batch is decoded. Between the call and
// the decode the caller still has that object and can draw into it, and a
// `fill(path)` means the path as it was when the call was made -- so touching
// anything in here hands the batch over first. See `guard`.
const referenced = new Set();
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
  referenced.clear();
  plot(target, arena, length, slots);
}

/** Room for `slots` more numbers, flushing another object's work first. */
function reserve(target, plot, slots) {
  if (owner !== target) {
    flush();
    owner = target;
    commit = plot;
  }
  if (used + slots > SLOTS) {
    // Hand over what is there rather than growing: a batch is as good in two
    // halves as in one.
    flush();
    owner = target;
    commit = plot;
  }
  const at = used;
  used += slots;
  return at;
}

//
// -- what a bad argument does ------------------------------------------------
//

/** `value` as a number, the way this binding reads one. */
function asNumber(value) {
  // `+value` throws for a symbol and a BigInt. Kept out of here on purpose:
  // this runs with a record half written, and a throw from inside it would
  // leave the arena holding reserved slots nothing will fill. The writer
  // refuses both before calling this, where it can `unreserve` first --
  // which is also where the message can name the argument.
  const kind = typeof value;
  if (kind === "symbol" || kind === "bigint") return NaN;
  return +value;
}

/** Records a value that is not a number, and answers where it went. */
const keep = (value) => values.push(value) - 1;

/**
 * Records a boxed handle, and remembers whose it is.
 *
 * Reads the handle itself rather than the draining accessor beside it:
 * `reserve` has already handed over anything this object had pending of its
 * own, and the record being written is by now half in the arena, so a flush
 * here would hand over an incomplete one. What the accessor exists to catch
 * -- the object being touched while a record still points at it -- is caught
 * by `referenced` instead, from the moment the record is whole.
 *
 * The private slot rather than `ø`, so a class that records its drawing is
 * not asked to hand its own batch over on the way past. It used to read one
 * or the other depending on which kind of class this was; both keep the
 * handle in the same place now.
 */
const keepHandle = (value) => {
  referenced.add(value);
  const held = value || {};
  return values.push(held[HANDLE]) - 1;
};

/**
 * Hands over a batch that would read `object` when it lands.
 *
 * For a class whose every crossing goes through the draining accessor this
 * happens by itself, and nothing needs to call this. `Image` is the
 * exception: its handle can be in a batch, but so is reading `complete`, and
 * a sprite loop reads that once per call -- draining there would end the
 * batch every time and leave the recording worth nothing. So an `Image`
 * drains at the one place its pixels are replaced rather than everywhere
 * they are read.
 */
const drain = (object) => {
  if (owner === object || referenced.has(object)) flush();
};

/**
 * Applies what `object` has recorded of its own, before a record reads it.
 *
 * A record resolves what it points at when the batch lands, and the batch
 * lands before any of it is applied -- so a verb pointing at the object that
 * is doing the recording would read it as it was before its own queue. That
 * is `path.addPath(path)`, and `ctx.drawImage(ctx.canvas, ...)`, which
 * copied a canvas that had not yet been painted.
 *
 * Narrower than [`drain`] on purpose, and called before a slot is reserved
 * rather than after. Draining for every object the batch merely points at
 * would end it on the second `fill(path)` of a loop, which is the shape
 * recording is for; and draining once the record is half written in the
 * arena would hand over half a record.
 */
const settle = (object) => {
  if (owner === object) flush();
};

/**
 * Records a list of numbers as it is now, rather than as it will be.
 *
 * Copied, where a handle is remembered: an array is ordinary JavaScript, so
 * `setLineDash(dash); dash[1] = 0` mutates it without crossing anything that
 * could hand the batch over first. A dash pattern is short and a stroke style
 * is set rarely, so the copy costs less than the interception would.
 */
const keepList = (value) => values.push(Array.prototype.slice.call(value)) - 1;

/** Gives back slots a refused call had reserved. */
const unreserve = (slots) => {
  used -= slots;
};

const ordinals = ["1st", "2nd", "3rd"];
const ordinal = (index) => ordinals[index] || `${index + 1}th`;

/**
 * Throws `error` as though the verb's own writer had thrown it.
 *
 * A recorded verb refuses an argument from a helper in this file, so without
 * this the first line of the stack names `drawlist.js` and the caller has to
 * read past the library to find their own call. The unrecorded half of the
 * binding has always trimmed itself out of the trace -- `argc` and
 * `rustError` both do -- and this is the same courtesy for the half that
 * records.
 *
 * `boundary` is the generated writer, so what is trimmed is the writer and
 * everything inside it, leaving the line that made the call on top. Costs
 * nothing until something throws.
 */
function raise(error, boundary) {
  Error.captureStackTrace(error, boundary || raise);
  throw error;
}

/** Refuses a call with fewer arguments than the verb reads. */
function checkArity(names, given, boundary) {
  if (given >= names.length) return;
  raise(
    new TypeError(
      `not enough arguments (missing: ${names.slice(given).join(", ")})`,
    ),
    boundary,
  );
}

// A rule an argument can carry, named by the schema. One check and one message
// each, shared by every verb that names it, so a verb added later writes no
// new error text.
// A rule an argument can carry, named by the schema. Consulted when a writer
// is generated, not when it runs: the check it produces is written into the
// function body.
const RULES = { non_negative: true };

// The kinds that are a number in the buffer: no kind at all, one kept at full
// width, and one carrying a rule. Everything else has a named branch in the
// writer below, and a kind in neither place is a schema this file has not
// caught up with.
const NUMERIC = new Set(["", "wide", "non_negative"]);

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
    const rules = spec.args.map((arg) => Boolean(RULES[arg.kind]));
    const { op, arity, flag } = spec;
    const count = names.length;

    // Generated rather than interpreted. A writer that decides per call
    // whether an argument is text, whether it carries a rule and whether
    // strict mode is on measured 24 nanoseconds for a `lineTo`; the same
    // decisions made here, once, and written into the function body, is what
    // closes the distance to the 14 a hand-written one costs.
    const slots = arity + 1;
    const body = [
      `if (arguments.length < ${count}) tooFew(names, arguments.length, self.at)`,
    ];
    for (let i = 0; i < count; i++) {
      const kind = spec.args[i].kind;
      if (kind === "handle" || kind === "image") {
        body.push(`settle(arguments[${i}])`);
      }
    }
    body.push(`var at = reserve(this, plot, ${slots})`);
    body.push(`arena[at] = ${op}`);
    for (let i = 0; i < count; i++) {
      const target = `arena[at + ${i + 1}]`;
      const kind = spec.args[i].kind;
      if (kind === "text") {
        // Refused at the call, because that is what the call has always
        // done: `string_arg` throws for anything that is not a string, and
        // without this the record simply pointed at a slot the decoder had
        // no string in and was dropped. `ctx.lineCap = 5` threw a TypeError
        // before these verbs were declared and silently did nothing after.
        //
        // A property that also takes an object never reaches here with one:
        // its writer is the `Text` form below, which declines and lets the
        // value cross the way it always did.
        body.push(
          `if (typeof arguments[${i}] !== "string") { unreserve(${slots}); refuseText(${i}, self.at) }`,
        );
        body.push(`${target} = keep(arguments[${i}])`);
      } else if (kind === "numbers") {
        // Refused at the call, as it was before there was a batch. A list
        // holding something that is not a number is a different matter: the
        // decoder drops that record, which is the silence it always had.
        body.push(
          `if (!Array.isArray(arguments[${i}])) { unreserve(${slots}); refuseSequence(self.at) }`,
        );
        body.push(`${target} = keepList(arguments[${i}])`);
      } else if (kind === "handle" || kind === "image") {
        body.push(`${target} = keepHandle(arguments[${i}])`);
      } else if (!NUMERIC.has(kind)) {
        // A kind declared in Rust that nothing here knows how to write. Said
        // now, loudly, because the alternative is what it did before this
        // check existed: fall through to the number below, coerce an object
        // to `NaN`, and leave the decoder dropping every record of that verb
        // in silence. An `image` argument shipped that way through a whole
        // test run before anything noticed.
        throw new Error(
          `no writer for a \`${kind}\` argument (${verb}.${names[i]})`,
        );
      } else {
        body.push(`var v${i} = num(arguments[${i}])`);
        // A `Symbol` or a `BigInt` has no numeric conversion at all, which
        // WebIDL makes a `TypeError` and a browser raises one for. It is not
        // the same failure as a non-finite number, which the Canvas API
        // ignores -- and this recorder used to give both the same answer,
        // because `num` hands back `NaN` for either. Refused whatever strict
        // mode says, since strict mode decides whether an *ignorable* value
        // is announced, and this one is not ignorable.
        body.push(
          `if (typeof arguments[${i}] === "symbol" || typeof arguments[${i}] === "bigint") ` +
            `{ unreserve(${slots}); refuseNumber(${i}, self.at) }`,
        );
        // Finite first, then the rule, which is the order the call checks
        // them in: a radius of `-Infinity` is not a number this can use, and
        // saying it must be positive answers a question that was never
        // reached. Where it is not finite and strict mode is off, the value
        // is written as it is and the decoder drops the whole record, which
        // is the silence the call has there.
        const refuse = rules[i]
          ? `if (v${i} < 0) { unreserve(${slots}); refuseRadius(self.at) }`
          : "";
        if (STRICT) {
          body.push(
            `if (!isFinite(v${i})) { unreserve(${slots}); refuseNumber(${i}, self.at) }` +
              (refuse ? `\nelse ${refuse}` : ""),
          );
        } else if (refuse) {
          body.push(`if (isFinite(v${i})) { ${refuse} }`);
        }
        body.push(`${target} = v${i}`);
      }
    }
    if (flag) {
      // A boolean, and only a boolean: `bool_arg_or` reads nothing else, so
      // `arc(x, y, r, a, b, 1)` sweeps clockwise here as it does on a call.
      body.push(
        `arena[at + ${count + 1}] = arguments[${count}] === true ? 1 : 0`,
      );
    }

    const here = {};
    const written_ = new Function(
      "arena",
      "reserve",
      "settle",
      "plot",
      "keep",
      "keepList",
      "keepHandle",
      "unreserve",
      "tooFew",
      "refuseRadius",
      "refuseNumber",
      "refuseSequence",
      "refuseText",
      "names",
      "num",
      "self",
      `return function ${verb.replace(/\W/g, "_")}(){\n${body.join("\n")}\n}`,
    )(
      arena,
      reserve,
      settle,
      plot,
      keep,
      keepList,
      keepHandle,
      unreserve,
      checkArity,
      (boundary) => {
        // The recorded half raises this itself rather than crossing, so it
        // does not pass through `rustError` and has to build the
        // `DOMException` here. The HTML Standard names `IndexSizeError` for a
        // negative radius; the two halves have to agree or the same mistake
        // gets two classes of error depending on whether the call was
        // recorded.
        raise(
          new DOMException("Radius value must be positive", "IndexSizeError"),
          boundary,
        );
      },
      (i, boundary) => {
        // No strict-mode marker: this is only ever thrown when strict mode
        // is on, so there is nothing left to decide by the time a caller
        // sees it. The call it stands for raises the marked message and has
        // the mark taken off on the way out, which is the same string.
        raise(
          new TypeError(
            `Expected a number for \`${names[i]}\` as ${ordinal(i)} arg`,
          ),
          boundary,
        );
      },
      (boundary) => {
        raise(new TypeError("Value is not a sequence"), boundary);
      },
      (i, boundary) => {
        raise(new TypeError(`Expected a string for \`${names[i]}\``), boundary);
      },
      names,
      asNumber,
      here,
    );
    // The boundary every refusal above trims the trace back to. A holder
    // rather than the function itself, because the writer cannot be handed
    // to its own factory while that factory is what builds it -- and it is
    // read only when something throws, so the indirection is never on the
    // path that draws.
    //
    // A `set_` verb is never installed as a method: it is reached through
    // `RustClass.prop`, so trimming back to the writer would leave this
    // file's own dispatch on top of the trace instead of the property the
    // caller assigned to. Trim to `prop` for those, which is where the
    // unrecorded half of the binding trims to as well.
    here.at = verb.startsWith("set_") ? RustClass.prototype.prop : written_;

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
    } else if (Object.hasOwn(klass.prototype, verb)) {
      // Replacing a method the class already has, never adding one. A verb
      // the class does not declare is reached by a wrapper that chooses
      // between shapes -- `fillPath2D` through `fill`, `drawImageAt` through
      // `drawImage` -- and that wrapper is where the argument is checked.
      // Installed here as well, those names became public methods that took
      // anything and silently drew nothing, where the call they stand for
      // says what was wrong with it.
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
const rawHandle = (target) => target[HANDLE];

/**
 * Makes reading `klass`'s boxed handle drain the arena first.
 *
 * The flush is the whole safety of this: Rust must never see an object with
 * verbs still pending. Every accessor calling `flush()` first is the usual way
 * to arrange that, and it fails the first time someone adds an accessor and
 * forgets. Reading the handle *is* the boundary, so the drain happens there
 * and "forgot to flush" stops being expressible.
 *
 * On the prototype, not on each object, which is where every wrapped class
 * now keeps this: defining a property per instance changes its shape, and
 * measured against a plain store into the private slot it costs 55 of the 83
 * nanoseconds the JavaScript half adds to a `new Path2D`. See `neon.HANDLE`.
 */
function guard(klass) {
  Object.defineProperty(klass.prototype, ø, {
    get() {
      // Either because this object has verbs of its own waiting, or because
      // the batch holds a record pointing at it and is about to read what it
      // finds there. Both have to be answered here: reading the handle is
      // the only way to reach this object from Rust, so a drain here is a
      // drain before anything can observe it or change it.
      drain(this);
      return this[HANDLE];
    },
    enumerable: false,
    configurable: true,
  });
  klass.records = true;
}

module.exports = { install, rawHandle, writerFor, drain };
