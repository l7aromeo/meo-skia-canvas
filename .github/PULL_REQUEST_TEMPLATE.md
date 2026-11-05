## What changed

<!-- What was wrong or missing, in the terms of someone who hit it, then what this does about it.
     Two or three sentences. "Fixes #N" for the issue it closes. -->

## Release impact

**Reaches**

- [ ] npm package
- [ ] Rust crate
- [ ] Neither: tests, CI or the repository only

**Breaking**

- [ ] Yes: working code gets a different outcome -- a value it passed is now refused or ignored,
      a value it reads back changes, or a signature or type changes
- [ ] No. A pixel correction is not breaking: it goes under Fixed.

**Changelog**

- [ ] A fragment, `changelog.d/npm/<n>.md` and/or `changelog.d/crate/<n>.md` with `<n>` this PR's
      number, for each channel it reaches: one line an entry, in the terms of someone using it and
      linking the issue or this PR; a Breaking entry adds one line saying what to change
- [ ] None: it reaches neither channel

<!-- A change that only makes something faster joins the release's one combined Changed entry
     rather than taking a line of its own: `- Joins **<start of its title>** (<links>)`.
     changelog.d/README.md has the form. -->

## Checks

- [ ] `just ci` passes -- the recipe is the authority on what that covers
- [ ] New behaviour is tested, and a fix's test fails without the fix
- [ ] Every new `unwrap`/`expect` under `src/` carries a `// SAFETY:` comment
- [ ] The docs and comments the change affects are updated in it, including those outside the diff
- [ ] Output that differs from Chrome is in `docs/browser-differences.md`, and explained where it
      diverges
- [ ] A new workaround for a dependency carries an `UPSTREAM:` marker
- [ ] A new platform target went into `lib/targets.json` only
- [ ] A new bundled dependency is in `THIRD-PARTY-NOTICES.md`
- [ ] No plans, notes or other process files are committed
