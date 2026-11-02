# Why the working rules say what they say

[AGENTS.md](../AGENTS.md) states each rule in a line or two. This page keeps
the incident behind each one: what happened, what it cost, and the rule it
produced. A rule reads as arbitrary without its reason, and the reason reads as
a story nobody needs at the keyboard, so the two are kept apart. Newest first.

A rule changes in AGENTS.md; an entry here does not change afterwards. It is a
record of why the rule was written, dated, and a later change to the rule gets
an entry of its own.

---

## 2026-09-08: a not-worked-around reason was an inference

Rule: [the reason on a `not worked around` marker is a claim, checked rather
than inferred](../AGENTS.md#upstream-defects-and-the-workarounds-for-them).

A marker drafted for #169's descendant half said the lengths were not
reachable from Rust. It inherited that from a comment saying the fix needed a
dpi `skia-safe` does not expose. Both premises were true and the conclusion did
not follow: the fix rewrites lengths to `px` and never needed the dpi, and
`skia-safe`'s `svg::Rect` declares `x`, `y`, `width` and `height` with setters
as well as getters. The marker would have published a mistake as a decision,
and shielded it from the re-check that would otherwise have corrected it.
Introduced in #176.

## 2026-09-08: workarounds landed unmarked and the gate said nothing

Rule: [the gate checks that every marker is well-formed, never that every
workaround is marked](../AGENTS.md#upstream-defects-and-the-workarounds-for-them).

A change adding workarounds went through with none of them marked, and
`scripts/check-upstream-notes.mjs` stayed green: a gate that validates markers
has nothing to say about their absence. The markers arrived in a later commit,
from someone reading the diff. Introduced in #180.

## 2026-09-08: "filed upstream" with nothing filed

Rule: [a marker names its issue, or says
`unfiled`](../AGENTS.md#upstream-defects-and-the-workarounds-for-them).

A comment said a defect was filed upstream, with no reference. It stopped a
re-check, because the defect looked handled; nothing had been filed. The issue
field is required so that the sentence cannot be written without the link, and
`unfiled` is a legitimate state written on the page rather than inferred from
an absence. Introduced in #175.

## 2026-09-08: nine readers kept a `get_` prefix for a reason that did not apply

Rule: [the naming exemption's file names are shorthand for a relationship, and
the relationship governs](../AGENTS.md#naming).

The getter exemption named `src/node`, `src/context/api.rs`, `src/canvas.rs`
and `src/context2d.rs`, where a `get_` prefix is half of an exported accessor
pair or mirrors a `getX()` method. Read as a list of places, it let nine plain
readers in `src/context/` keep the prefix with no `set_` counterpart, no
collision and no `getX()` to mirror. Introduced in #195.

## 2026-09-07: the Rust suite went unrun inside `just ci`

Rule: [a recipe's name says what it covers](../AGENTS.md#build-test-and-development).

A recipe called `test` ran one language's suite, and `just ci`, which called
it, ran no Rust tests for a while; everything stayed green. `test` now runs
both, and the halves are `test-rust` and `test-js`. The same pass renamed the
two API gates for what they check: `check-rust-api`, which fails when a public
Rust signature leaks a type from a hidden crate, and `check-dts-surface`, which
fails when `lib/index.d.ts` disagrees with the built addon. They had been a
word apart, and being confused for each other cost time more than once.

## 2026-09-07: comments went stale the day they were written

Rule: [change the code and change its comments in the same
commit](../AGENTS.md#comments-say-what-the-code-does-not-what-it-used-to-do).

A sweep of the tree for wrong statements found most of them had been written
the same day, by the work that had just moved the code underneath them. Two
were in a header whose own author had not revisited it after a later change of
theirs falsified it. The comment that goes stale is usually not in the diff: a
doc block sits above a function, and three lines of context show none of it.
Each missed comment costs a whole-tree sweep to find again, because nothing
distinguishes it from a maintained one.

The same sweep is where the search-pattern failures in
[testing.md](internals/testing.md#search-patterns) were found.

## 2026-08-14: a BMP header said `sRGB` and wrote `sWin`

Rule: [a number or byte string from a specification gets a named constant and
a doc comment saying where it came from](../AGENTS.md#no-magic-values).

A BMP header carried `0x7357_696E` under a comment reading `// "sRGB "`. Those
four bytes spell `sWin`: the front of `LCS_sRGB` joined to the back of
`LCS_WINDOWS_COLOR_SPACE`, a value the format does not define. It shipped and
survived review, because readers ignore the field and every viewer showed the
right picture. A literal under a comment is a claim nobody can check; a named
constant whose doc comment gives `wingdi.h`'s name for it is one anyone can.
The rule was written in f1002ca1.
