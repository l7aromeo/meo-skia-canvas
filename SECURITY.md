# Security policy

## Reporting a vulnerability

Report privately through GitHub's
[security advisories](https://github.com/l7aromeo/meo-skia-canvas/security/advisories/new).
Please do not open a public issue for anything exploitable.

Expect an acknowledgement within a week. This is a small project, so a fix may take longer than that
— you will be told where it stands rather than left waiting.

## What is in scope

This package ships a native module that decodes untrusted input. The parts most worth scrutiny:

- **Image and font decoding.** `loadImage`, `FontLibrary` and the `Image` component all parse
  attacker-controllable bytes through Skia. Memory-safety bugs reachable from a decoder are in
  scope.
- **The install path.** `lib/prebuild.mjs` downloads a binary over the network and verifies it
  against the SHA-256 hashes in `package.json`. Anything that lets a wrong or unverified binary be
  installed is in scope.
- **Platform package resolution.** `lib/binary.js` chooses which prebuilt binary to load. Loading
  something other than the intended package is in scope.

The crate reaches the same decoders through a Rust API rather than through Node, so a
memory-safety bug behind `loadImage` or font parsing is in scope from either surface. The install
path and platform resolution are npm-only: Cargo builds from source or from `skia-safe`'s own
prebuilt Skia, and neither goes through `lib/prebuild.mjs`.

## What is not

- Vulnerabilities in Skia itself. Report those to
  [the Skia project](https://skia.org); this package will pick up the fix through `skia-safe`.
- Denial of service from deliberately enormous canvases or images. Rendering is bounded by memory,
  and callers are expected to bound their own input sizes.
- Anything requiring the attacker to already control the machine running the render.

## Supported versions

The latest minor release receives fixes. Older lines are not backported.

There are two release lines and they are not comparable: the npm package `meo-skia-canvas`
continues the upstream `skia-canvas` numbering, and the crate of the same name on crates.io started
at `0.1.0`. "Latest minor" means the latest of whichever line you depend on. A fix that touches
Rust reaches both, but not necessarily in the same week -- a change to the build container is an npm
release with no crate release, which is the common case.

## How releases are published

Nothing here is published from a developer machine, and no long-lived publish credential exists for
either registry.

Every npm package -- the main one and the seven platform packages -- is published from GitHub
Actions through [npm trusted publishing](https://docs.npmjs.com/trusted-publishers) with an OIDC
credential and provenance attestation. Verify a published artefact with:

```bash
npm audit signatures
```

The crate is published the same way, by `crates-io-publish.yml` through
[crates.io trusted publishing](https://crates.io/docs/trusted-publishing): the workflow exchanges
its OIDC identity for a token that expires, rather than holding one in a repository secret.

Workflow actions are pinned to commit SHAs rather than tags in every workflow that can publish, so
a moved tag cannot introduce new code into the release path. Dependabot advances those pins.
