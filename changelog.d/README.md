# Changelog fragments

A pull request that reaches a package adds its changelog entries here, in a
file of its own, instead of editing `CHANGELOG-npm.md` or `CHANGELOG-crate.md`.
Edited directly, those two files are shared by nearly every pull request, so
each merge would leave the rest needing `main` merged in and CI run again.

- `npm/<n>.md` for the Node addon, `crate/<n>.md` for the Rust crate, `<n>`
  being the pull request's number. A change reaching both writes both, each for
  that changelog's reader.
- A fragment holds sections and entries in the changelog's own form, one line
  each, linking the pull request or its issue. A Breaking entry adds one
  indented line saying what to change.

```markdown
### Fixed

- **A drop shadow keeps its blur's tail along the canvas edge its offset moves it away from.** ([#467](https://github.com/l7aromeo/meo-skia-canvas/pull/467), [#454](https://github.com/l7aromeo/meo-skia-canvas/issues/454))
```

- The sections are `Breaking`, `Security`, `Fixed`, `Added`, `Changed` and
  `Known, and not fixed here`.
- A speed change with no change in output joins the release's one entry saying
  what costs less, naming it by the start of its title and giving its links:

```markdown
### Changed

- Joins **Text costs less** ([#601](https://github.com/l7aromeo/meo-skia-canvas/pull/601))
```

`node scripts/changelog-fragments.mjs check`, run by `just check-changelog` and
in CI, refuses a fragment the changelog would refuse, a section it does not
have, and a join that finds no entry or two.

When a release is prepared, `just changelog-assemble <tag>` (`npm-v7.0.0`,
`rust-v0.17.0`) writes the channel's fragments into its changelog, newest first
in each section, merges the joins' links into their entries, and removes the
fragments. The release's opening paragraphs, and any count they state, are
written by hand after that. `just release-npm` and `just release-crate` refuse
to run while their channel holds a fragment.
