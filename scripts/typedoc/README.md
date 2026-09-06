# The JavaScript API reference

`lib/index.d.ts` is the published contract — it ships in the package, it is
what an editor shows on hover, and the static tests already read it as the
truth about what this library offers. This turns it into a browsable
reference, so the same file answers the question in an editor, in the
package, and on the web. It is the counterpart to `cargo doc` for the Rust
half, and `just docs` runs both.

Only the reference is generated. The narrative pages under `docs/` —
getting started, the guides, the measured comparisons — are written by hand
and stay that way. A generator has nothing to say about them.

## The two hand-written files here

`index.md` becomes the entry page, and `theme.css` is the skin. Both are
inputs to the build rather than output, and neither is in `docs/`, because
both describe the reference rather than the library.

**`index.md`** exists because `readme: "none"` left the entry page as a
heading and two module links — less than the npm page says. What belongs on
it is the orientation only this page can give: what the library is, one
example that reaches pixels, and the difference between the `index` and
`browser` entry points, which is the thing the two links underneath it
actually mean. It is not a second copy of `docs/index.md`, and pulling guide
material into it is how it becomes one.

It links into the API with `{@link}` rather than by URL, so `invalidLink`
validation resolves every one against the real declarations and the build
fails when a symbol it names is renamed or removed. Prefer that form over a
hand-written path for anything inside the reference — a plain link rots in
silence.

**`theme.css`** overrides the theme's colour tokens and little else. The
palette is not chosen there: it is copied from `docs/generate/brand.js`, the
script that draws the hero banners, so the reference and the banner at the
top of the README agree. `build.mjs` checks that rather than asserting it —
every colour `brand.js` draws with has to appear as some `--brand-*`, or the
build stops and names the one that does not. It compares values and not
names, because the mapping from `THEMES.hero.tile` to `--brand-tile` is not
mechanical and a table of those pairs would be a third thing to drift. The file's own comments carry the reasoning,
including which contrast measurement forced which value. Two things worth
knowing before editing it: TypeDoc wraps its whole stylesheet in
`@layer typedoc`, so unlayered rules here win without `!important` and
without matching its specificity; and the kind-icon colours are left alone
on purpose, because they are a legend rather than decoration.

## Why this has its own `package.json`

The root project is on **TypeScript 7**, which is the native port. Its main
entry point is `lib/version.cjs`, and the classic compiler API — the
`SyntaxKind` and `createProgram` that every documentation generator is built
on — exists only under its `./unstable/*` subpaths:

```
$ node -e "console.log(require('typescript').version, typeof require('typescript').SyntaxKind)"
7.0.2 undefined
```

So no version of TypeDoc can read this repository's TypeScript. The stable
line refuses to install against it, and the `1.0.0-dev` line installs and
then throws `Cannot read properties of undefined (reading
'TypeAliasDeclaration')` on startup.

Rather than pin the whole project back to TypeScript 5 for the sake of a
documentation tool, or carry `--legacy-peer-deps` in every install, the tool
gets its own directory with its own dependency tree. `.d.ts` syntax is
stable, so TypeScript 5.9 parses the declarations this repository writes
without knowing anything about the compiler that type-checks them.

`scripts/api-surface` pins its own copy for the same reason and reads the
same file, so this is a pattern with two instances rather than an exception
made for the documentation build.

That isolation is also the thing to check first when this breaks: the
version here has nothing to do with the version in the root
`package.json`, and it does not need to.

## How the reference is grouped

`@category` tags in `lib/*.d.ts` decide the headings; `categoryOrder` here
decides what order they come out in, and without it they come out
alphabetical, which puts Context Mixins second — directly under Canvas, when
it is the heading a newcomer should reach last. So the taxonomy lives in the
declarations and its ordering lives here, and the two have to be edited
together: a category added there and not listed here falls to the `*` and
lands among the unordered ones, without anything reporting it.

One entry sits **after** the `*`, and that is deliberate rather than a typo.
`Shared with the Node Build` is the `browser` module's leftovers — the
re-exports it takes from Node unchanged — so it belongs last whatever else
appears. Before the `*` it would sort ahead of any category nobody has
ordered yet; after it, it stays at the bottom. Verified by giving a member a
category absent from the list and watching which side of it landed.

`categorizeByGroup` is off because it categorises _within_ each kind group,
so ten categories across five kinds gave a module page of 33 headings —
"Classes - Canvas", "Interfaces - Canvas", "Type Aliases - Canvas" — most
holding one or two entries. Off, it is the ten categories themselves.

`navigation.includeGroups` stays **on**, which is not obvious and was measured
rather than reasoned about. It reads like the switch that would restore the
33-heading cross-product in the sidebar, and it does not: with
`categorizeByGroup` off, categories win there whichever way it is set, and
all it does is decide what happens in a module that has _no_ categories. With
it off, such a module renders as a flat alphabetical list of every export —
163 lines on a tree without the tags. With it on, that module keeps its
Classes/Interfaces/Type Aliases grouping and a categorised one is unaffected.
So it costs nothing and is the difference between degrading gracefully and
degrading badly.

## Running it

```sh
just docs-js      # build it
just docs         # both halves, Rust and JavaScript
```

Output lands in `target/jsdoc`, beside `target/doc` where `cargo doc` puts
the Rust half. Both are build products and neither is committed.

## The gate

`notDocumented` validation is on, which is the JavaScript counterpart to the
crate's `#![warn(missing_docs)]`. It is a ratchet rather than a switch: the
count may fall and it may hold, and a build that raises it fails and says by
how much. `undocumented-baseline.txt` is where it stands.

**It stands at zero**, so the ratchet is at its tightest and one undocumented
member is a red build. That is not a warning about strictness — it is the
whole point of a ratchet, and it got there the ordinary way, by people
lowering it. When this said the gate "does not fail the build yet" and named
258 undocumented members of 621, both halves had stopped being true and a
contributor reading it would have landed an undocumented export expecting a
printed number and got a failure instead.
