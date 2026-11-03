//! One declaration per drawing verb, for every consumer of it.
//!
//! A verb is written down once -- the name JavaScript calls it, the arguments
//! it takes, and the code that applies it -- and this generates both the entry
//! point Node calls today and the arm a decoder will need when those calls are
//! batched instead of made one at a time. Two callers, one body, so the pair
//! cannot disagree about what `lineTo` means or how many numbers it takes.
//!
//! What that is worth, measured on this machine: a `lineTo` costs 82
//! nanoseconds, of which 17 is the crossing itself, 39 is reading two floats
//! out of the arguments, 20 is unboxing and borrowing the receiver, and 6 is
//! the JavaScript wrapper. A drawing frame of `examples/node/animated-eye.js`
//! makes 1319 such calls, and sets a property 4915 times.
//!
//! `verb_kind` below names every argument kind a declaration can ask for
//! beyond a bare number: a non-negative one, a full-width `f64`, an enum
//! name, a number sequence, an image and a handle. Each carries its own
//! check and its own message, because the error behaviour differs by kind
//! and `tests/suite/arguments.test.js` pins it. A call whose arguments no
//! kind describes stays hand-written at its declaration site.

use std::ops::Range;

/// The argument kinds a declaration can ask for, beyond "a number".
///
/// One check and one message per kind, shared by every verb that names it, so
/// adding a verb writes no new error text. These are the kinds this binding
/// already enforced by hand before they were written down.
pub(crate) mod verb_kind {
    /// Rejects a radius below zero, as a browser does.
    pub(crate) mod non_negative {
        /// What a caller is told.
        ///
        /// The name in front is not prose: `lib/classes/neon.js` reads it and
        /// raises a `DOMException` called that, which is what the HTML
        /// Standard specifies for a negative radius and what a browser
        /// throws. Neon can construct an `Error`, a `TypeError` and a
        /// `RangeError` and nothing else, so the name has to cross as text.
        pub(crate) const MESSAGE: &str =
            "IndexSizeError: Radius value must be positive";
    }

    /// Whether `value` breaks the rule its argument was declared with.
    ///
    /// Checked on the number as JavaScript gave it, before anything narrows,
    /// so a value too large for an `f32` is refused for what it is rather than
    /// for the infinity it would become. Taken by reference because the same
    /// call shape has to serve a kind whose argument is a `String`.
    pub(crate) fn non_negative(value: &f64) -> bool {
        *value < 0.0
    }

    /// Carries no rule. Names the arguments kept at full width -- alpha,
    /// which this fork keeps as a double rather than truncating to a byte.
    ///
    /// Every kind answers both questions a declaration can ask of it: whether
    /// a value breaks its rule, and what to say when it does. This one has no
    /// rule, so it always answers no and its message is never reached.
    pub(crate) mod wide {
        /// Unreachable: [`super::wide`] refuses nothing.
        pub(crate) const MESSAGE: &str = "a number";
    }

    /// Whether `value` breaks this kind's rule, which it cannot.
    pub(crate) fn wide<T>(_value: T) -> bool {
        false
    }

    /// Carries no rule either. Names an argument that is a string: a colour,
    /// a font, the name of an enum. It travels beside the numbers rather than
    /// in them, and what it must be is decided where it is used -- an
    /// unrecognised enum name leaves a property alone rather than failing.
    pub(crate) mod text {
        /// Unreachable: [`super::text`] refuses nothing.
        pub(crate) const MESSAGE: &str = "a string";
    }

    /// Whether `value` breaks this kind's rule, which it has none of.
    pub(crate) fn text<T>(_value: T) -> bool {
        false
    }

    /// Names an argument that is a list of numbers -- a dash pattern -- read
    /// out of the array it was given when the record was written.
    pub(crate) mod numbers {
        /// Unreachable: [`super::numbers`] refuses nothing.
        pub(crate) const MESSAGE: &str = "a sequence";
    }

    /// Whether `value` breaks this kind's rule, which it has none of.
    pub(crate) fn numbers<T>(_value: T) -> bool {
        false
    }

    /// Names an argument that is an image to paint -- an `Image` or the
    /// context of a `Canvas` -- carried beside the numbers as the content it
    /// resolved to when the call was made.
    pub(crate) mod image {
        /// Unreachable: [`super::image`] refuses nothing.
        pub(crate) const MESSAGE: &str = "an Image or a Canvas";
    }

    /// Whether `value` breaks this kind's rule, which it has none of.
    pub(crate) fn image<T>(_value: T) -> bool {
        false
    }

    /// Names an argument that is a `Path2D`, carried beside the numbers as a
    /// copy of the path it held when the call was made.
    pub(crate) mod handle {
        /// Unreachable: [`super::handle`] refuses nothing.
        pub(crate) const MESSAGE: &str = "a Path2D";
    }

    /// Whether `value` breaks this kind's rule, which it has none of.
    pub(crate) fn handle<T>(_value: T) -> bool {
        false
    }
}

use neon::prelude::*;
use skia_safe::Path;

use crate::node::{image::Source, path::BoxedPath2D};

/// A value a record refers to by index, because it is not a number.
///
/// The buffer carrying a batch is `f64` throughout, which is what a JavaScript
/// number is and what makes the writers cheap. A colour, a font or a
/// composite-operation name has to travel beside it, so those go into an array
/// and the record holds the index.
#[derive(Clone, Debug)]
pub(crate) enum Slot {
    /// A string: a CSS colour, a font, an enum name.
    Text(String),
    /// A list of numbers -- a dash pattern -- copied out of the array the
    /// call was given, at the moment it was given.
    ///
    /// The copy is made on the JavaScript side rather than here, because an
    /// array is ordinary JavaScript: `setLineDash(dash); dash[1] = 0` changes
    /// it without crossing anything that could hand the batch over first.
    Numbers(Vec<f32>),
    /// A path, copied out of the `Path2D` a record pointed at.
    ///
    /// Read when the batch arrives rather than when the record was written --
    /// a record holds the object, not the path. What makes those the same
    /// path is arranged on the JavaScript side: touching a `Path2D` that a
    /// pending record points at hands the batch over first, so nothing can
    /// draw into it in between. Copied rather than borrowed because the
    /// caller owns it again the moment this returns.
    Path(Path),
    /// An image to paint, resolved from whatever the call was given.
    ///
    /// Resolved when the batch arrives rather than when the record was
    /// written, as a path is, and unchanged in between for the same reason:
    /// reaching the `Image` or the `Canvas` behind it means reading its
    /// handle, and reading a handle a pending record points at hands the
    /// batch over first.
    Image(Source),
    /// Something this decoder has no use for, which is a writer's mistake.
    Unusable,
}

impl Slot {
    /// The string this slot holds, if it holds one.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Slot::Text(value) => Some(value),
            _ => None,
        }
    }

    /// The numbers this slot holds, if it holds a list of them.
    pub(crate) fn numbers(&self) -> Option<&[f32]> {
        match self {
            Slot::Numbers(value) => Some(value),
            _ => None,
        }
    }

    /// The image this slot holds, if it holds one.
    pub(crate) fn image(&self) -> Option<&Source> {
        match self {
            Slot::Image(value) => Some(value),
            _ => None,
        }
    }

    /// The path this slot holds, if it holds one.
    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Slot::Path(value) => Some(value),
            _ => None,
        }
    }
}

/// The strings a batch carries: one string crossing for all of them, as
/// UTF-8, cut at each one's length in UTF-16 units, which is how JavaScript
/// counts a string -- see `keepText` in `lib/classes/drawlist.js`.
///
/// Read as UTF-8 because that is what a string read from JavaScript on its
/// own was, and V8 writes a one-byte string out as UTF-8 at about the cost of
/// a copy, where widening it to UTF-16 and decoding that back cost a batch of
/// labels more than reading each label apart had. Each piece is a range of the
/// joined string rather than a string of its own.
///
/// V8 writes a lone surrogate as U+FFFD, one unit as it was. A lone high
/// surrogate ending one text and a lone low one beginning the next join into
/// a pair, which UTF-8 holds as one character of two units; a cut inside one
/// gives each side U+FFFD, as reading the two texts apart did.
pub(crate) struct Texts {
    joined: String,
    pieces: Vec<Piece>,
}

/// One text of a batch: where it lies in the joined string, and whether it
/// begins or ends with the half of a pair the cut went through, written as
/// U+FFFD beside the range.
enum Piece {
    Range(Range<usize>),
    Split(String),
}

impl Texts {
    /// `joined` cut at `lengths`, in UTF-16 units. A length running past the
    /// end takes what is left, which is a writer's mistake, and the texts
    /// after it are empty.
    pub(crate) fn cut(joined: String, lengths: &[u32]) -> Self {
        let pieces = match joined.is_ascii() {
            // A unit is a byte.
            true => {
                let mut at = 0_usize;
                lengths
                    .iter()
                    .map(|&length| {
                        let end = at
                            .saturating_add(length as usize)
                            .min(joined.len());
                        let piece = Piece::Range(at..end);
                        at = end;
                        piece
                    })
                    .collect()
            }
            false => Self::walk(&joined, lengths),
        };
        Self { joined, pieces }
    }

    /// [`Self::cut`] for a string that is not all ASCII: its characters
    /// walked, counting the units each takes.
    fn walk(joined: &str, lengths: &[u32]) -> Vec<Piece> {
        let mut chars = joined.char_indices().peekable();
        // The cut before this text went through a pair: its first unit is the
        // low half, already stepped over.
        let mut low_half = false;
        lengths
            .iter()
            .map(|&length| {
                let mut units = length as usize;
                let opens_split = low_half && units > 0;
                if opens_split {
                    low_half = false;
                    units -= 1;
                }
                let start = chars.peek().map_or(joined.len(), |&(at, _)| at);
                let mut end = start;
                let mut closes_split = false;
                while units > 0 {
                    let Some(&(at, c)) = chars.peek() else { break };
                    let width = c.len_utf16();
                    if width > units {
                        // The cut lands between the halves of this pair.
                        closes_split = true;
                        low_half = true;
                        chars.next();
                        break;
                    }
                    units -= width;
                    end = at + c.len_utf8();
                    chars.next();
                }
                match (opens_split, closes_split) {
                    (false, false) => Piece::Range(start..end),
                    _ => Piece::Split(
                        [
                            opens_split.then_some(
                                char::REPLACEMENT_CHARACTER.to_string(),
                            ),
                            Some(joined[start..end].to_owned()),
                            closes_split.then_some(
                                char::REPLACEMENT_CHARACTER.to_string(),
                            ),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                    ),
                }
            })
            .collect()
    }

    fn get(&self, ordinal: usize) -> Option<&str> {
        self.pieces.get(ordinal).map(|piece| match piece {
            Piece::Range(range) => &self.joined[range.clone()],
            Piece::Split(text) => text.as_str(),
        })
    }
}

/// What a batch carries beside its numbers: the values array, and the texts.
///
/// A record names a text by -1 less its ordinal, so that it cannot be taken
/// for an index into the values. A non-negative index naming a string reads
/// one from the values, which a batch written by hand -- `verbs.test.js`
/// writes its own -- can still carry.
pub(crate) struct Slots {
    values: Vec<Slot>,
    texts: Texts,
}

impl Slots {
    pub(crate) fn new(values: Vec<Slot>, texts: Texts) -> Self {
        Self { values, texts }
    }

    /// The value at `index`.
    pub(crate) fn get(&self, index: usize) -> Option<&Slot> {
        self.values.get(index)
    }

    /// The string a record names at `at`.
    pub(crate) fn text(&self, at: f64) -> Option<&str> {
        match at < 0.0 {
            true => self.texts.get((-1.0 - at) as usize),
            false => self.values.get(at as usize).and_then(Slot::text),
        }
    }
}

/// Reads the values array a batch carries beside its numbers.
///
/// Read in full before the buffer is borrowed: pulling a string out of
/// JavaScript needs the context, and the decode loop holds a slice that
/// borrows it.
pub(crate) fn read_slots(
    cx: &mut FunctionContext,
    values: Handle<JsArray>,
) -> NeonResult<Vec<Slot>> {
    let raw = values.to_vec(cx)?;
    let mut slots = Vec::with_capacity(raw.len());
    for value in raw {
        slots.push(if let Ok(text) = value.downcast::<JsString, _>(cx) {
            Slot::Text(text.value(cx))
        } else if let Ok(path) = value.downcast::<BoxedPath2D, _>(cx) {
            Slot::Path(path.borrow().path())
        } else if let Some(source) = Source::of(cx, value) {
            Slot::Image(source)
        } else if let Ok(list) = value.downcast::<JsArray, _>(cx) {
            // Every element or nothing: a list with something unusable in it
            // is not a list of numbers, and the verb reading it says what to
            // do about that rather than this deciding here.
            let raw = list.to_vec(cx)?;
            let numbers: Vec<f32> = raw
                .iter()
                .filter_map(|value| {
                    value
                        .downcast::<JsNumber, _>(cx)
                        .ok()
                        .map(|number| number.value(cx) as f32)
                })
                .collect();
            match numbers.len() == raw.len() {
                true => Slot::Numbers(numbers),
                false => Slot::Unusable,
            }
        } else {
            Slot::Unusable
        });
    }
    Ok(slots)
}

/// Reads one argument off a call, as the kind it was declared with.
///
/// A number is read as a double and narrowed where it lands; a `text` argument
/// is read as a string, which the batched path carries in a slot instead. The
/// messages are the ones this binding has always given.
macro_rules! read_arg {
    ($cx:expr, $at:expr, $name:expr,) => {
        $crate::node::utils::double_arg_or_bail($cx, $at, $name)?
    };
    ($cx:expr, $at:expr, $name:expr, wide) => {
        $crate::node::utils::double_arg_or_bail($cx, $at, $name)?
    };
    ($cx:expr, $at:expr, $name:expr, non_negative) => {
        $crate::node::utils::double_arg_or_bail($cx, $at, $name)?
    };
    ($cx:expr, $at:expr, $name:expr, text) => {
        $crate::node::utils::string_arg($cx, $at, $name)?
    };
    ($cx:expr, $at:expr, $name:expr, numbers) => {
        match $crate::node::utils::opt_float_vec_arg($cx, $at) {
            Some(numbers) => numbers,
            None => return $cx.throw_type_error("Value is not a sequence"),
        }
    };
    // Anything that is not an image resolves to a content that paints
    // nothing, which is what this call has always done with one.
    ($cx:expr, $at:expr, $name:expr, image) => {
        $crate::node::utils::opt_image_source_arg($cx, $at).unwrap_or_default()
    };
    ($cx:expr, $at:expr, $name:expr, handle) => {
        match $crate::node::utils::opt_skpath_arg($cx, $at) {
            Some(path) => path,
            None => {
                return $cx.throw_type_error(format!(
                    "Expected a Path2D for {} arg",
                    $crate::node::utils::arg_num($at)
                ))
            }
        }
    };
}

/// Narrows an argument read straight off a call.
macro_rules! narrow {
    ($value:expr,) => {
        $value as f32
    };
    ($value:expr, wide) => {
        $value
    };
    ($value:expr, non_negative) => {
        $value as f32
    };
    // Borrowed rather than owned, so a body reads the same string type
    // whichever path it arrived by. The `String` it borrows is the shadowed
    // binding, which outlives the borrow.
    ($value:expr, text) => {
        $value.as_str()
    };
    // Already a path, whichever way it arrived.
    ($value:expr, handle) => {
        $value
    };
    // Already resolved, whichever way it arrived.
    ($value:expr, image) => {
        $value
    };
    // Already a list, whichever way it arrived.
    ($value:expr, numbers) => {
        $value
    };
}

/// Narrows an argument to what its verb expects.
///
/// Skia is `f32` throughout, so that is the default. An argument declared
/// `@ wide` keeps the `f64` JavaScript gave, which is what `globalAlpha`
/// needs -- see `crate::gpu` on why this fork keeps float alpha.
macro_rules! bind_arg {
    ($value:expr, $slots:expr,) => {
        $value as f32
    };
    ($value:expr, $slots:expr, wide) => {
        $value
    };
    ($value:expr, $slots:expr, non_negative) => {
        $value as f32
    };
    // The number is an index; the value is beside it. A record pointing at a
    // slot holding the wrong thing is a broken writer, and the verb is skipped
    // rather than applied to something invented.
    ($value:expr, $slots:expr, numbers) => {
        match $slots
            .get($value as usize)
            .and_then($crate::node::verbs::Slot::numbers)
        {
            Some(numbers) => numbers,
            None => return None,
        }
    };
    ($value:expr, $slots:expr, image) => {
        match $slots
            .get($value as usize)
            .and_then($crate::node::verbs::Slot::image)
        {
            Some(source) => source.clone(),
            None => return None,
        }
    };
    ($value:expr, $slots:expr, handle) => {
        match $slots
            .get($value as usize)
            .and_then($crate::node::verbs::Slot::path)
        {
            Some(path) => path.clone(),
            None => return None,
        }
    };
    ($value:expr, $slots:expr, text) => {
        match $slots.text($value) {
            Some(text) => text,
            None => return None,
        }
    };
}

/// Declares a drawing verb once, for both of its callers.
///
/// Each entry reads `jsName as Opcode (args) => |receiver| { body }`. The
/// arguments are bound as `f32` by the names given, so the body reads as it
/// would if it had been written by hand.
macro_rules! verbs {
    (
        // The receiver these verbs apply to: the opcode type to declare, the
        // boxed handle JavaScript passes as its first argument, and the type
        // behind it that the bodies below operate on.
        $enum:ident for $boxed:ty => $target:ty;
        $(
            $js:ident as $op:ident
            ( $($arg:ident $(@ $kind:ident)?),* )
            $(; $flag:ident)?
            => |$this:ident| $body:block
        ),* $(,)?
    ) => {
        /// The verbs this receiver accepts, as opcodes.
        ///
        /// `#[repr(u8)]` because these cross to JavaScript as numbers and back
        /// again; the discriminant is the wire value.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        #[allow(dead_code)]
        pub(crate) enum $enum { $($op),* }

        #[allow(dead_code)]
        impl $enum {
            /// Every verb: the name JavaScript uses, its opcode, and how many
            /// numbers it reads.
            pub(crate) const ALL: &'static [(&'static str, $enum, usize)] =
                &[$((
                    stringify!($js),
                    $enum::$op,
                    <[&str]>::len(&[$(stringify!($arg),)* $(stringify!($flag),)?]),
                )),*];

            /// How many numbers this verb reads.
            pub(crate) fn arity(self) -> usize {
                match self {
                    $($enum::$op => <[&str]>::len(
                        &[$(stringify!($arg),)* $(stringify!($flag),)?]
                    )),*
                }
            }

            /// The verb an opcode names, or `None` for a value that is not one.
            ///
            /// Indexed, not searched. The discriminants are 0 upwards in
            /// declaration order, which is the order `ALL` is in, so the
            /// opcode is the index -- and the discriminant is checked again
            /// on the way out, so this stays right if that ever stops being
            /// true. It was a linear scan, which the decoder ran once per
            /// record: at fifty-odd verbs that is twenty-five comparisons a
            /// record, and `fillRect` measured 119 ns through a batch
            /// against 107 before the last dozen verbs were declared.
            pub(crate) fn from_code(code: u8) -> Option<Self> {
                Self::ALL
                    .get(code as usize)
                    .map(|(_, op, _)| *op)
                    .filter(|op| *op as u8 == code)
            }
        }

        /// Applies one decoded verb to its receiver.
        ///
        /// `None` when `args` is not the length the verb reads, which is a
        /// malformed record rather than a bad argument -- the caller has
        /// already been told what a bad argument is.
        #[allow(dead_code)]
        pub(crate) fn apply(
            op: $enum,
            target: &mut $target,
            args: &[f64],
            #[allow(unused_variables)] slots: &$crate::node::verbs::Slots,
        ) -> Option<()> {
            match op {
                $($enum::$op => {
                    // The flag rides in the record as a number, after the
                    // arguments, which is why the arity below counts it.
                    if let [$($arg,)* $($flag,)?] = args {
                        // A record that breaks a rule is dropped rather than
                        // reported: the writer refused it at the call, so one
                        // arriving here is a broken writer, not a caller.
                        $($(
                            if $crate::node::verbs::verb_kind::$kind($arg) {
                                return None;
                            }
                        )?)*
                        $(let $arg = $crate::node::verbs::bind_arg!(*$arg, slots, $($kind)?);)*
                        $(let $flag = *$flag != 0.0;)?
                        let $this = target;
                        $body
                        Some(())
                    } else {
                        None
                    }
                })*
            }
        }

        /// The verbs, for JavaScript to generate its own writers from.
        ///
        /// `{ name: { op, arity, args: [{ name, kind }], flag } }`. Exported
        /// rather than written out on the JavaScript side, so the two cannot
        /// disagree about an opcode, an order, or a rule -- the failure that
        /// would produce is a drawing that is quietly wrong rather than an
        /// error.
        #[allow(non_snake_case)]
        pub fn verbTable(mut cx: FunctionContext) -> JsResult<JsObject> {
            let table = cx.empty_object();
            $({
                let entry = cx.empty_object();
                let op = cx.number($enum::$op as u8);
                entry.set(&mut cx, "op", op)?;
                let arity = cx.number($enum::$op.arity() as f64);
                entry.set(&mut cx, "arity", arity)?;

                let args = cx.empty_array();
                // Unused for a verb that takes none, which is four of them.
                #[allow(unused_mut, unused_variables)]
                let mut at = 0u32;
                $({
                    let arg = cx.empty_object();
                    let name = cx.string(stringify!($arg));
                    arg.set(&mut cx, "name", name)?;
                    // Empty where the argument carries no rule beyond being a
                    // number, which is most of them.
                    let kind = cx.string(concat!("" $(, stringify!($kind))?));
                    arg.set(&mut cx, "kind", kind)?;
                    args.set(&mut cx, at, arg)?;
                    // The final increment is never read, which clippy is
                    // right about and which is what a counted loop looks like.
                    #[allow(unused_assignments)]
                    {
                        at += 1;
                    }
                })*
                entry.set(&mut cx, "args", args)?;

                // Whether a counter-clockwise flag follows the numbers.
                let flag = cx.boolean(
                    !<[&str]>::is_empty(&[$(stringify!($flag),)?]),
                );
                entry.set(&mut cx, "flag", flag)?;

                let key = cx.string(stringify!($js));
                table.set(&mut cx, key, entry)?;
            })*
            Ok(table)
        }

        /// Applies a batch of verbs recorded by JavaScript.
        ///
        /// The buffer holds `opcode, args..., opcode, args...`, and the length
        /// says how much of it is written. Everything is `f64` on the way over
        /// because that is what a JavaScript number is; the arguments narrow to
        /// `f32` here, as they do on the one-at-a-time path.
        pub fn plot(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let this = cx.argument::<$boxed>(0)?;
            let buffer = cx.argument::<JsFloat64Array>(1)?;
            let len = cx.argument::<JsNumber>(2)?.value(&mut cx) as usize;
            // Read whole, before the buffer is borrowed: pulling a string out
            // of JavaScript needs the context that the slice below holds.
            //
            // Absent where the batch held nothing a buffer of numbers could
            // not carry, which is most batches -- a run of line segments and
            // property writes names no string, path or image. The argument
            // was being handed over and walked anyway, and the crossing is
            // what a flush costs: applying nothing at all took 732 of the 797
            // nanoseconds a one-verb batch did, and the buffer's own length
            // made no difference to either.
            //
            // The texts first: they share a buffer with the next batch's, and
            // reading a value can reach back into JavaScript, which could
            // begin one.
            let texts = match cx.len() > 5 {
                true => {
                    let joined = cx.argument::<JsString>(4)?.value(&mut cx);
                    let lengths = cx.argument::<JsUint32Array>(5)?;
                    let lengths =
                        neon::types::buffer::TypedArray::as_slice(&*lengths, &cx);
                    $crate::node::verbs::Texts::cut(joined, lengths)
                }
                false => $crate::node::verbs::Texts::cut(String::new(), &[]),
            };
            let values = match cx.len() > 3 {
                true => {
                    let values = cx.argument::<JsArray>(3)?;
                    $crate::node::verbs::read_slots(&mut cx, values)?
                }
                false => Vec::new(),
            };
            let slots = $crate::node::verbs::Slots::new(values, texts);

            // Borrowed and released before anything can throw: the slice holds
            // `&cx`, and reporting an error needs `&mut cx`.
            let outcome = {
                let data = neon::types::buffer::TypedArray::as_slice(&*buffer, &cx);
                let data = &data[..len.min(data.len())];
                let mut target = this.borrow_mut();
                let mut at = 0;
                let mut outcome = Ok(());

                while at < data.len() {
                    let code = data[at] as u8;
                    let Some(op) = $enum::from_code(code) else {
                        outcome = Err(format!("unknown drawing verb {code}"));
                        break;
                    };
                    let arity = op.arity();
                    let Some(args) = data.get(at + 1..at + 1 + arity) else {
                        outcome = Err(format!(
                            "a {} record was cut short",
                            stringify!($enum)
                        ));
                        break;
                    };
                    at += 1 + arity;

                    // A verb carrying a coordinate it cannot use does nothing,
                    // which is what the same call does when made on its own.
                    if !args.iter().all(|n| n.is_finite()) {
                        continue;
                    }

                    apply(op, &mut target, args, &slots);
                }
                outcome
            };

            match outcome {
                Ok(()) => Ok(cx.undefined()),
                Err(why) => cx.throw_error(why),
            }
        }

        $(
            #[allow(non_snake_case)]
            pub fn $js(mut cx: FunctionContext) -> JsResult<JsUndefined> {
                let this = cx.argument::<$boxed>(0)?;
                let mut this = this.borrow_mut();
                // Arity first, and with every name, so a short call is
                // refused the way it always was.
                //
                // Saturating, for the reason `_float_args_at` gives: the
                // receiver is argument 0 and nothing outside this crate is
                // obliged to pass one, since the addon's exports are
                // reachable directly. A bare subtraction wraps to
                // `usize::MAX` on a call carrying none, which passes the
                // check below and then indexes `NAMES` out of range. The
                // `cx.argument` above happens to refuse such a call first,
                // so this was safe by a neighbouring line rather than by
                // construction -- and it is the arity check, which has no
                // business depending on the order of the two.
                const NAMES: &[&str] = &[$(stringify!($arg)),*];
                let given = cx.len().saturating_sub(1);
                if given < NAMES.len() {
                    let missing = NAMES[given..].join(", ");
                    return cx.throw_type_error(format!(
                        "not enough arguments (missing: {missing})"
                    ));
                }
                // Counts up as each argument is read; unused by a verb that
                // takes none, which is four of them.
                #[allow(unused_mut, unused_variables)]
                let mut reading = 0usize;
                $(
                    reading += 1;
                    let $arg = $crate::node::verbs::read_arg!(
                        &mut cx,
                        reading,
                        stringify!($arg),
                        $($kind)?
                    );
                )*
                let _ = reading;
                // Counted once, outside the optional flag below: the two
                // repeat over different things and cannot share an expansion.
                const ARITY: usize = <[&str]>::len(&[$(stringify!($arg)),*]);
                $(
                    // The flag follows the numbers and defaults to false, as
                    // the Canvas API says.
                    let $flag = bool_arg_or(&mut cx, ARITY + 1, false);
                )?
                $($(
                    if $crate::node::verbs::verb_kind::$kind(&$arg) {
                        return cx.throw_range_error(
                            $crate::node::verbs::verb_kind::$kind::MESSAGE,
                        );
                    }
                )?)*
                $(
                    let $arg =
                        $crate::node::verbs::narrow!($arg, $($kind)?);
                )*
                let $this = &mut *this;
                $body
                Ok(cx.undefined())
            }
        )*
    };
}

pub(crate) use bind_arg;
pub(crate) use narrow;
pub(crate) use read_arg;
pub(crate) use verbs;

#[cfg(test)]
mod tests {
    use super::Texts;

    /// `texts` as JavaScript hands them over: joined and written out as
    /// UTF-8 the way V8 writes a string, a lone surrogate as U+FFFD, with
    /// each one's length in UTF-16 units.
    fn crossed(texts: &[&[u16]]) -> (String, Vec<u32>) {
        (
            String::from_utf16_lossy(&texts.concat()),
            texts.iter().map(|text| text.len() as u32).collect(),
        )
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Each text comes back as it went in: a NUL inside one, a character
    /// outside the Basic Multilingual Plane, an empty one between two others
    /// and one at the end of the batch, and text that is not ASCII, whose
    /// UTF-8 is longer than its UTF-16.
    #[test]
    fn joined_texts_are_cut_where_each_one_ends() {
        let texts = ["a\0b", "😀x", "", "é", "#336699", "", "end😀"];
        let pieces: Vec<Vec<u16>> =
            texts.iter().map(|text| utf16(text)).collect();
        let (joined, lengths) =
            crossed(&pieces.iter().map(Vec::as_slice).collect::<Vec<_>>());
        let cut = Texts::cut(joined, &lengths);
        for (at, text) in texts.iter().enumerate() {
            assert_eq!(cut.get(at), Some(*text), "text {at}");
        }
        assert_eq!(cut.get(texts.len()), None, "nothing past the last");
    }

    /// A text ending in the high half of a pair and the next beginning with
    /// the low half are each a lone surrogate, as they were when each was
    /// read on its own -- U+FFFD each -- and not the pair the join makes.
    #[test]
    fn a_pair_split_across_two_texts_stays_two_lone_halves() {
        let [high, low] = [0xD83D_u16, 0xDE00];
        let (joined, lengths) =
            crossed(&[&[u16::from(b'a'), high], &[low, u16::from(b'b')]]);
        let cut = Texts::cut(joined, &lengths);
        assert_eq!(cut.get(0), Some("a\u{FFFD}"));
        assert_eq!(cut.get(1), Some("\u{FFFD}b"));
    }

    /// The halves of a pair the join makes, each a text of its own or with an
    /// empty text between them, and a lone half beside a whole pair: each
    /// half is U+FFFD where it was, and the pair stays a pair.
    #[test]
    fn a_cut_through_a_pair_puts_each_half_where_it_was() {
        let [high, low] = [0xD83D_u16, 0xDE00];
        let a = u16::from(b'a');
        let (joined, lengths) =
            crossed(&[&[high], &[], &[low], &[high, low, high], &[low, a]]);
        let cut = Texts::cut(joined, &lengths);
        let want = ["\u{FFFD}", "", "\u{FFFD}", "😀\u{FFFD}", "\u{FFFD}a"];
        for (at, text) in want.iter().enumerate() {
            assert_eq!(cut.get(at), Some(*text), "text {at}");
        }
    }

    /// A lone surrogate inside a text, which V8 writes as U+FFFD in three
    /// bytes, takes one unit, so the texts after it start where they did.
    #[test]
    fn a_lone_surrogate_inside_a_text_takes_one_unit() {
        let (joined, lengths) = crossed(&[
            &[u16::from(b'x'), 0xDC00, u16::from(b'y')],
            &utf16("next"),
        ]);
        let cut = Texts::cut(joined, &lengths);
        assert_eq!(cut.get(0), Some("x\u{FFFD}y"));
        assert_eq!(cut.get(1), Some("next"));
    }

    /// A length past the end takes what is left, and the texts after it are
    /// empty rather than read from somewhere else.
    #[test]
    fn a_length_past_the_end_takes_what_is_left() {
        let cut = Texts::cut("abc".to_owned(), &[2, 5, 1]);
        assert_eq!(cut.get(0), Some("ab"));
        assert_eq!(cut.get(1), Some("c"));
        assert_eq!(cut.get(2), Some(""));
        let cut = Texts::cut("aé".to_owned(), &[1, 5, 1]);
        assert_eq!(cut.get(1), Some("é"), "the same where it is not ASCII");
        assert_eq!(cut.get(2), Some(""));
    }
}
