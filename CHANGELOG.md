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

---

## Where the history is

Every past release is in the channel file it shipped on. A release that
carried both -- 19 of the 58 -- is in both, under the heading it was cut with.

|       | releases | file                                     |
| ----- | -------- | ---------------------------------------- |
| crate | 23       | [CHANGELOG-crate.md](CHANGELOG-crate.md) |
| npm   | 54       | [CHANGELOG-npm.md](CHANGELOG-npm.md)     |

The 33 oldest carry no channel marking because they predate the crate
entirely: they run from August 2020 to May 2026 and the first crate tag is
August 2026, so they are npm's.

Within a release that carried both, the entries were **not** separated. The
heading recorded the channel; the entries never did, and assigning one to each
now would mean deciding from memory rather than from the code.
