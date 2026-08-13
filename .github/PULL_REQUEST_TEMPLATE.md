## What this changes

<!-- The problem, then the fix. Two or three sentences. -->

## Why it belongs in this fork

<!-- Most of this code is inherited, but this is where it is maintained: phyron is dormant and this
     tree does not merge from samizdatco, so a fix with no home upstream still belongs here. Say
     what the change is for, not why it could not go elsewhere. -->

## Checks

- [ ] `just ci` passes (`fmt-check typecheck lint-check test build`)
- [ ] Any new `unwrap`/`expect` carries a `// SAFETY:` comment
- [ ] A new platform target, if added, went into `lib/targets.json` only
- [ ] A new bundled dependency, if added, is in `THIRD-PARTY-NOTICES.md`
