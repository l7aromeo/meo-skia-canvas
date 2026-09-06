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
a Rust caller and part of a larger story to a JavaScript one, each file
groups it the way its own reader would look for it, so an entry on one side
may answer to a clause on the other rather than to a whole entry.

---

## Where the history is

**Released history is in [CHANGELOG-npm.md](CHANGELOG-npm.md), all of it**,
under the heading `Releases before the channels were separated`.
[CHANGELOG-crate.md](CHANGELOG-crate.md) currently carries the unreleased
block alone, so a crate reader looking for anything already published is in
the wrong file. This is a gap rather than a decision, and it is the one thing
about the split that is not yet finished.

That history holds 54 release sections. Each heading names the channels it
shipped on:

- 19 carry both, as `[v5.8.0] (npm) / [v0.14.0] (crate)`.
- 2 carry `(npm)` alone -- `v5.4.0` and `v4.1.0`, which had no crate release.
- 33 carry no marking at all. Those are npm's, and predate the crate: they
  run from August 2020 to May 2026, where the first crate tag is
  `rust-v0.3.0` in August 2026. **One exception sits among them** --
  `[crates.io 0.1.0]`, dated May 14 2026, which is a crate release, carries
  no `(crate)` marking, and is filed out of date order.

Four crate releases have a link definition at the foot of the file but no
section of their own: `0.5.0`, `0.11.0`, `0.12.1` and `0.13.0`. `0.5.0` is
described elsewhere as crate-only; the other three are named only in the
comparison links.

Within a release that carried both channels, most entries are not separated
by channel: the heading records what shipped and the individual entries
mostly do not. Five sections are the exception and do say so on their own --
`Crate 0.8.0 -- breaking`, `Crate 0.7.0 -- breaking`, `Crate 0.6.0 --
breaking`, `Crate 0.6.0 -- new` and `Crate 0.3.1`. Separating the rest now
would mean deciding from memory rather than from the code.
