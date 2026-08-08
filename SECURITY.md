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

## What is not

- Vulnerabilities in Skia itself. Report those to
  [the Skia project](https://skia.org); this package will pick up the fix through `skia-safe`.
- Denial of service from deliberately enormous canvases or images. Rendering is bounded by memory,
  and callers are expected to bound their own input sizes.
- Anything requiring the attacker to already control the machine running the render.

## Supported versions

The latest minor release receives fixes. Older lines are not backported.

## How releases are published

Every npm package here is published from GitHub Actions through
[npm trusted publishing](https://docs.npmjs.com/trusted-publishers) with an OIDC credential and
provenance attestation. No long-lived publish token exists to be stolen. Verify a published
artefact with:

```bash
npm audit signatures
```
