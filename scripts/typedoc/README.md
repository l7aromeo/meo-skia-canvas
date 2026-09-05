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
top of the README agree. The file's own comments carry the reasoning,
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

That isolation is also the thing to check first when this breaks: the
version here has nothing to do with the version in the root
`package.json`, and it does not need to.

## Running it

```sh
just docs-js      # build it
just docs         # both halves, Rust and JavaScript
```

Output lands in `target/jsdoc`, beside `target/doc` where `cargo doc` puts
the Rust half. Both are build products and neither is committed.

## The gate

`notDocumented` validation is on, which is the JavaScript counterpart to the
crate's `#![warn(missing_docs)]`. It does not fail the build yet: 258 of the
621 members carry no documentation, and turning it red today would only
teach everyone to pass `--skipErrorChecking`. The count is printed instead,
and `undocumented-baseline.txt` records where it stood. Lower it.
