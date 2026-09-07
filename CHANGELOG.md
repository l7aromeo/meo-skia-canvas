# Changelog

This project ships two things from one tree, on two release channels with
separate numbering. Neither is the primary one and neither file is the
default: each channel has its own changelog, and this file is the index.

| channel                      | changelog                                | published to                                      |
| ---------------------------- | ---------------------------------------- | ------------------------------------------------- |
| Rust crate `meo-skia-canvas` | [CHANGELOG-crate.md](CHANGELOG-crate.md) | crates.io, from `0.2.0`                           |
| Node addon `meo-skia-canvas` | [CHANGELOG-npm.md](CHANGELOG-npm.md)     | npm, continuing `phyron-skia-canvas` from `3.6.0` |

The versions are not comparable. A change touching only the binding is an npm
release with no crate release, which is the common case; a change to the
rendering engine is usually both.

**A change that affects both surfaces gets an entry in both files**, written
for that audience rather than copied. The same commit is a new `match` arm to
a Rust caller and a changed property value to a JavaScript one, and neither
reader should have to read the other's half to find out what happened to them.

The two surfaces are not always the same size. Where a change is one thing to
a Rust caller and part of a larger story to a JavaScript one, each file groups
it the way its own reader would look for it, so an entry on one side may
answer to a clause on the other rather than to a whole entry.

---

## Where the history is

**Each file holds its own channel's releases, all the way back.** That rule
covers the history as well as the unreleased block: a release that shipped on
both channels is written in both files, under the heading it was cut with.

| file                                     | released sections |
| ---------------------------------------- | ----------------- |
| [CHANGELOG-crate.md](CHANGELOG-crate.md) | 24                |
| [CHANGELOG-npm.md](CHANGELOG-npm.md)     | 54                |

Those two numbers are gated by `just check-changelog`, which counts the
headings rather than trusting the table.

The crate's 24 are the 19 dual-channel releases, plus `crates.io 0.1.0`, its
first publication, which carried no npm release, plus four more that shipped
on the crate alone: `0.5.0`, `0.11.0`, `0.12.1` and `0.13.0`. The npm file's
54 are the same 19, plus `v5.4.0`, `v4.1.0` and `v3.7.0` which had no crate
release, plus 32 that predate the crate entirely -- they run from August 2020
to May 2026, where the first crate tag is `rust-v0.3.0` in August 2026.

**The four crate-only releases are why the two counts cannot be reconciled by
subtraction.** A reader adding 19 and one and expecting the crate's total will
be four short; the crate shipped on its own five times counting `0.1.0`, and
those releases have no npm heading to sit beside. They are in
[CHANGELOG-crate.md](CHANGELOG-crate.md) and nowhere else, which is what the
rule at the top of this section requires.

## What the older entries do and do not say

Inside a release that shipped on both channels, the entries are mostly not
separated by channel. Nine places are the exception, and **every one of them
marks the crate**: five `Crate 0.8.0 -- breaking` style sections, one
`**Crate only**` line, and three entries carrying an italic `_(Rust only)_`
after the bold lead.

**Nothing marks the other direction.** There is no `(npm only)`, `(JS only)`
or `(binding only)` anywhere in that history. So an unmarked entry in a
dual-channel release means one of two things -- it affected both surfaces, or
it affected npm alone -- and which one is not recoverable from the file.
Those releases are reproduced whole in both files rather than filtered,
because deciding what to drop would mean deciding from memory rather than
from the code.
