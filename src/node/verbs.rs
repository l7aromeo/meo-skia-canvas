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
//! Only verbs whose arguments are all numbers live here. The ones taking a
//! path, an image, a string or a sequence answer to different rules -- and
//! measurably different error behaviour, which
//! `tests/suite/arguments.test.js` pins -- so they stay hand-written until
//! there is somewhere in the queue to put a handle.

/// The argument kinds a declaration can ask for, beyond "a number".
///
/// One check and one message per kind, shared by every verb that names it, so
/// adding a verb writes no new error text. These are the kinds this binding
/// already enforced by hand before they were written down.
pub(crate) mod verb_kind {
    /// Rejects a radius below zero, as a browser does.
    pub(crate) mod non_negative {
        /// What a caller is told, which is what they were told before.
        pub(crate) const MESSAGE: &str = "Radius value must be positive";
    }

    /// Whether `value` breaks the rule its argument was declared with.
    pub(crate) fn non_negative(value: f32) -> bool {
        value < 0.0
    }
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
            pub(crate) fn from_code(code: u8) -> Option<Self> {
                Self::ALL
                    .iter()
                    .map(|(_, op, _)| *op)
                    .find(|op| *op as u8 == code)
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
            args: &[f32],
        ) -> Option<()> {
            match op {
                $($enum::$op => {
                    // The flag rides in the record as a number, after the
                    // arguments, which is why the arity below counts it.
                    if let [$($arg,)* $($flag,)?] = args {
                        $(let $arg = *$arg;)*
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

        $(
            #[allow(non_snake_case)]
            pub fn $js(mut cx: FunctionContext) -> JsResult<JsUndefined> {
                let this = cx.argument::<$boxed>(0)?;
                let mut this = this.borrow_mut();
                let [$($arg),*] = float_args_or_bail_n(
                    &mut cx,
                    &[$(stringify!($arg)),*],
                )?;
                // Counted once, outside the optional flag below: the two
                // repeat over different things and cannot share an expansion.
                const ARITY: usize = <[&str]>::len(&[$(stringify!($arg)),*]);
                $(
                    // The flag follows the numbers and defaults to false, as
                    // the Canvas API says.
                    let $flag = bool_arg_or(&mut cx, ARITY + 1, false);
                )?
                $($(
                    if $crate::node::verbs::verb_kind::$kind($arg) {
                        return cx.throw_range_error(
                            $crate::node::verbs::verb_kind::$kind::MESSAGE,
                        );
                    }
                )?)*
                let $this = &mut *this;
                $body
                Ok(cx.undefined())
            }
        )*
    };
}

pub(crate) use verbs;
