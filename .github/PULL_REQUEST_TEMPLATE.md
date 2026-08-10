## What this changes

<!-- The problem, then the fix. Two or three sentences. -->

## Why it belongs in this fork

<!-- Most of this code is inherited. If the change is not specific to how the native binary is
     packaged or resolved, it probably belongs upstream at phyrondev/phyron-skia-canvas. -->

## Checks

- [ ] `just ci` passes (`fmt-check typecheck lint-check test build`)
- [ ] Any new `unwrap`/`expect` carries a `// SAFETY:` comment
- [ ] A new platform target, if added, went into `lib/targets.json` only
- [ ] A new bundled dependency, if added, is in `THIRD-PARTY-NOTICES.md`
