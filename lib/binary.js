//
// Native binary resolution
//

"use strict";

const { existsSync } = require("fs"),
  { resolve } = require("path");

// Prebuilt binaries published as ordinary npm packages, one per target. Each declares `os`, `cpu`
// and (on Linux) `libc`, so the package manager installs exactly the one matching the host and
// skips the rest — that is what makes them optional dependencies rather than dependencies.
//
// `targets.json` is the single source of truth: the release tooling and the `optionalDependencies`
// block are generated from it, so a target can only ever be added in one place.
const TARGETS = require("./targets.json");

const packageName = (triplet) => `meo-skia-canvas-${triplet}`;

// Only Linux is ambiguous once os and cpu are known, and only between the two libc flavours. Both
// are probed because the one that does not apply was never installed, so nothing has to detect the
// libc at load time.
const PLATFORM_PACKAGES = Object.entries(TARGETS).reduce((byPlatform, [triplet, target]) => {
  const [platform] = target.os,
    [arch] = target.cpu,
    arches = byPlatform[platform] || (byPlatform[platform] = {});
  (arches[arch] || (arches[arch] = [])).push(packageName(triplet));
  return byPlatform;
}, {});

// The path the `install` script downloads to. Still the fallback: a consumer who installed before
// the platform packages existed, or who vendored the binary by hand, keeps working untouched.
const LOCAL_BINARY = resolve(__dirname, "skia.node");

// A missing platform package can surface as MODULE_NOT_FOUND or, when the package is present but
// its subpath is not exported, as ERR_PACKAGE_PATH_NOT_EXPORTED. Neither is worth distinguishing
// here, so every resolution failure simply moves on to the next candidate.
function loadPlatformPackage() {
  const candidates = (PLATFORM_PACKAGES[process.platform] || {})[process.arch] || [];

  for (const name of candidates) {
    try {
      return require(`${name}/skia.node`);
    } catch {
      continue;
    }
  }

  return null;
}

// Reported when nothing resolves, because the default failure — a bare MODULE_NOT_FOUND for
// `../skia.node` — says nothing about why the binary is absent. Overwhelmingly the cause is an
// install script that never ran: bun blocks postinstall scripts unless the package is listed in
// `trustedDependencies`, and `--ignore-scripts` does the same on every other package manager.
function missingBinaryError() {
  const triplet = [process.platform, process.arch].join("-");

  return new Error(
    `meo-skia-canvas: no native binary for ${triplet}.\n` +
      `Reinstall to fetch one, or run \`npm rebuild meo-skia-canvas\`.\n` +
      `If you install with bun, add "trustedDependencies": ["meo-skia-canvas"] to your package.json.`,
  );
}

function loadSkiaNode() {
  const fromPackage = loadPlatformPackage();

  if (fromPackage) {
    return fromPackage;
  } else if (existsSync(LOCAL_BINARY)) {
    return require(LOCAL_BINARY);
  } else {
    throw missingBinaryError();
  }
}

module.exports = { TARGETS, PLATFORM_PACKAGES, packageName, loadSkiaNode };
