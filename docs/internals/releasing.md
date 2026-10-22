# How a release moves through CI

What is worth knowing while cutting a release. The rules for releasing are in
[AGENTS.md](../../AGENTS.md#releases), and the `justfile` recipes are the
authority on the steps.

## The draft window

**A draft release stops the rendering suite, and that is not a failure.** A
draft's assets are not downloadable, so `ci.yml` cannot fetch a binary between
the tag and the publish. It handles this with a notice and stays green. A red
`ci.yml` in that window is therefore something else.

## The Lambda archives

**The Lambda archives embed a packed copy of the module**, so they cannot be
reused across a version bump without repacking. The platform binaries have no
such problem.

## What LFS cost

LFS is metered on both storage and bandwidth, accumulates every version ever
pushed, and has no eviction -- so what it costs is never recovered. Commits made
before the switch still hold pointers, which is why `git-lfs` remains worth
having installed for reading history.
