#!/usr/bin/env node
// Archives a patched Skia build in the layout skia-bindings downloads, so a
// crate built from crates.io can link this repository's Skia by pointing
// `SKIA_BINARIES_URL` at the archives.
//
//   node scripts/skia-pack.mjs pack <build-dir> <out-dir> <target> <features> [static]
//       pack a skia-bindings build that already ran: <features> are
//       skia-bindings' cargo features, comma-separated
//   node scripts/skia-pack.mjs build [--expect <key>] <out-dir> <target> [cargo args]
//       build the crate in release for <target> with those cargo args, then
//       pack the skia-bindings build it ran, its features and static C
//       runtime as cargo and rustc report them; `--expect` refuses an
//       archive under any other key
//   node scripts/skia-pack.mjs plan
//       print the archives this checkout publishes, as JSON: one per target,
//       crate feature set and C runtime, with the key cargo predicts
//   node scripts/skia-pack.mjs published
//       refuse unless the release skia-binaries.txt names holds every planned
//       archive
//   node scripts/skia-pack.mjs upload <archive> <release tag>
//       upload an archive to the release, never replacing one it holds:
//       a held name with the same bytes is left as it is, with others refused
//   node scripts/skia-pack.mjs consumer <target-dir> patched|stock
//       refuse a crate build in <target-dir> that did not link the Skia it
//       was meant to: the archives and this crate's patch cfgs, or
//       crates.io's Skia and none of them, both downloaded, never compiled
//   node scripts/skia-pack.mjs origin
//       print the skia-bindings version Cargo.lock names and the rust-skia
//       commit its crates.io package was made from, space-separated
//   node scripts/skia-pack.mjs manifest [--check]
//       write skia-binaries.txt, or check the one in the tree
//   node scripts/skia-pack.mjs --self-test
//
// skia-bindings downloads `skia-binaries-{key}.tar.gz` from
// `SKIA_BINARIES_URL` (build_support/binary_cache/env.rs), `{key}` being
// `<rust-skia commit, 20 hex>-<target>-<features>[-static][-debug]`
// (binary_cache/binaries.rs `key`), and unpacks its `skia-binaries/`
// directory into its out-dir. The archive holds what its `export` writes: the
// libraries the build links, `bindings.rs`, and the files the features need
// beside them (binaries_config.rs). The key names the rust-skia commit
// crates.io's skia-bindings was packaged from, read from that package's
// `.cargo_vcs_info.json`, and not this repository's patches: the release the
// archives are uploaded to names those.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildScriptLogs } from "./check-patched-skia.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// The directory the archive holds and skia-bindings unpacks
/// (`binaries::ARCHIVE_NAME`).
const ARCHIVE_NAME = "skia-binaries";

/// Hex digits of the rust-skia commit in a key (`git::HALF_HASH_LENGTH`).
const HALF_HASH_LENGTH = 20;

/// skia-bindings' features that are part of a key, as
/// `Features::from_cargo_env` reads them (build_support/features.rs). Cargo
/// features outside this list -- `binary-cache`, `embed-icudtl`, `jpeg`,
/// `webp`, `freetype` and the like -- name these or nothing, and are not.
const KEY_FEATURES = [
  "pdf",
  "gl",
  "egl",
  "wayland",
  "x11",
  "vulkan",
  "metal",
  "d3d",
  "ganesh",
  "graphite",
  "textlayout",
  "svg",
  "skottie",
  "webp-encode",
  "webp-decode",
  "embed-freetype",
  "freetype-woff2",
  "jpeg-encode",
  "jpeg-decode",
];

/// The shorter names a key gives some features (`feature::KEY_REPLACEMENTS`).
const KEY_REPLACEMENTS = {
  "webp-encode": "webpe",
  "webp-decode": "webpd",
  "embed-freetype": "ftembed",
  "freetype-woff2": "ftwoff2",
  "jpeg-encode": "jpege",
  "jpeg-decode": "jpegd",
};

/// The features that only a target using FreeType keeps
/// (`feature::FREETYPE_SPECIFIC`): skia-bindings drops them from the build and
/// from the key elsewhere (platform.rs `redundant_features`).
const FREETYPE_SPECIFIC = ["embed-freetype", "freetype-woff2"];

/**
 * Whether skia-bindings builds `target` with FreeType (each platform's
 * `uses_freetype` in build_support/platform/): not on Apple's platforms,
 * Windows or OpenHarmony; on Linux, musl, Android, Emscripten and anything
 * else.
 */
export function usesFreetype(target) {
  if (/-apple-/.test(target)) return false;
  if (target.includes("-windows-")) return false;
  if (target.endsWith("-linux-ohos")) return false;
  return true;
}

/// The data file a Windows build with `textlayout` needs beside its libraries
/// (`binaries_config::ICUDTL_DAT`).
const ICUDTL_DAT = "icudtl.dat";

/**
 * The key skia-bindings computes for a build of `target` with cargo
 * features `features`, packaged from rust-skia commit `commit`.
 */
export function key(commit, target, features, crtStatic = false) {
  const named = features
    .filter((feature) => KEY_FEATURES.includes(feature))
    .filter(
      (feature) => usesFreetype(target) || !FREETYPE_SPECIFIC.includes(feature),
    )
    .map((feature) => KEY_REPLACEMENTS[feature] ?? feature)
    .sort();
  return [
    commit.slice(0, HALF_HASH_LENGTH),
    target,
    ...(named.length ? [named.join("-")] : []),
    ...(crtStatic ? ["static"] : []),
  ].join("-");
}

/** Whether `target` is a Windows one, whose libraries are `.lib` files. */
const isWindows = (target) => target.includes("-windows-");

/** What a static library named `name` is called on `target`. */
export function libraryFile(target, name) {
  return isWindows(target) ? `${name}.lib` : `lib${name}.a`;
}

/**
 * The libraries a skia-bindings build makes for `features`, in the order it
 * links them: binaries_config.rs `from_features` and `built_libraries`. A
 * download unpacks each of them and fails on one missing, so the archive
 * holds this list exactly.
 */
export function builtLibraries(features) {
  const has = (feature) => features.includes(feature);
  return [
    ...(has("textlayout")
      ? ["skparagraph", "skshaper", "skunicode_core", "skunicode_icu"]
      : []),
    ...(has("svg") ? ["svg", "skresources"] : []),
    ...(has("skottie")
      ? [
          "skottie",
          "sksg",
          "jsonreader",
          ...(has("svg") ? [] : ["skresources"]),
        ]
      : []),
    "skia",
    "skia-bindings",
  ];
}

/**
 * The files the archive of a build carries, in `out/skia`: the libraries
 * `builtLibraries` names, `bindings.rs`, and `icudtl.dat` on Windows with
 * `textlayout`.
 *
 * The build's `output` has to link those libraries, so that an archive cannot
 * be packed for features other than the build's, and it does so the way
 * skia-bindings prints them: one run of consecutive lines in that order
 * (binaries_config.rs `commit_to_cargo`), each `rustc-link-lib=static=<name>`,
 * or on Windows, where `static=` breaks the link, `rustc-link-lib=<name>`
 * (cargo.rs `add_static_link_lib`). Every other line names a library the
 * target links from the system, before the run or after it -- a musl
 * build's fontconfig brings expat both from pkg-config's probe and from
 * `link_libraries` -- and a download links those again on the consumer's
 * machine, so none of them is packed.
 */
export function archiveFiles(output, target, features) {
  const expected = builtLibraries(features);
  const form = isWindows(target) ? "" : "static=";
  const lines = [...output.matchAll(/^cargo:rustc-link-lib=(.+)$/gm)].map(
    (m) => m[1],
  );
  const run = expected.map((name) => `${form}${name}`);
  const at = lines.findIndex((_, start) =>
    run.every((line, i) => lines[start + i] === line),
  );
  if (at < 0)
    throw new Error(
      `skia-pack: the build's output does not link ${run.join(", ")} in one run, as skia-bindings prints the libraries it builds for [${features}]; it links ${lines.join(", ") || "nothing"}`,
    );
  const extra =
    isWindows(target) && features.includes("textlayout") ? [ICUDTL_DAT] : [];
  return [
    ...expected.map((name) => libraryFile(target, name)),
    "bindings.rs",
    ...extra,
  ];
}

/**
 * Packs the skia-bindings build in `buildDir` into `outDir` as
 * `skia-binaries-<key>.tar.gz`, once `vouch` finds nothing wrong with its
 * log. `how` is `{ commit, target, features, crtStatic, version }`. Returns
 * `{ archive, files }`: the archive's path, and each file it holds as
 * `{ name, bytes }`.
 */
export function pack(buildDir, outDir, how, vouch) {
  const output = readFileSync(buildScriptLogs(buildDir).output, "utf8");
  const why = vouch(output);
  if (why) throw new Error(`skia-pack: ${buildDir} is not packed: ${why}`);
  const skia = join(buildDir, "out", "skia");
  const files = archiveFiles(output, how.target, how.features);
  const missing = files.filter((file) => !existsSync(join(skia, file)));
  if (missing.length)
    throw new Error(`skia-pack: ${skia} holds no ${missing.join(", ")}`);
  const name = key(how.commit, how.target, how.features, how.crtStatic);
  const staging = mkdtempSync(join(tmpdir(), "skia-pack-staging-"));
  try {
    const dir = join(staging, ARCHIVE_NAME);
    mkdirSync(dir);
    for (const file of files)
      writeFileSync(join(dir, file), readFileSync(join(skia, file)));
    writeFileSync(join(dir, "tag.txt"), how.version);
    writeFileSync(join(dir, "key.txt"), name);
    const held = [...files, "tag.txt", "key.txt"].map((file) => ({
      name: file,
      bytes: statSync(join(dir, file)).size,
    }));
    // Named relative to the staging directory, with no drive letter for GNU
    // tar -- the one first on PATH in a Windows runner's bash -- to read
    // `D:\...` as a remote `host:path`.
    execFileSync("tar", ["-czf", archiveName(name), ARCHIVE_NAME], {
      cwd: staging,
    });
    mkdirSync(outDir, { recursive: true });
    const archive = join(outDir, archiveName(name));
    copyFileSync(join(staging, archiveName(name)), archive);
    return { archive, files: held };
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

/**
 * Whether `rustc --print cfg` output `cfg` has the static C runtime on:
 * skia-bindings reads the same fact from `CARGO_CFG_TARGET_FEATURE`
 * (cargo.rs `target_crt_static`) and keys by it. It is on by default for
 * Rust's musl targets and off for MSVC, so it is read, not assumed.
 */
export function crtStaticIn(cfg) {
  return /^target_feature="crt-static"$/m.test(cfg);
}

/**
 * The `--target` a build of `target` on `host` passes cargo: none where the
 * target is the host, so RUSTFLAGS reach skia-bindings' build script as they
 * do in build.yml's builds -- given `--target`, cargo passes them to the
 * target alone, and a musl build script would then link statically, where
 * bindgen cannot load libclang -- and none where `cargoArgs` already name
 * one, as a build with `-Zhost-config` must.
 */
export function targetArgs(target, host, cargoArgs) {
  if (target === host) return [];
  if (
    cargoArgs.some((arg) => arg === "--target" || arg.startsWith("--target="))
  )
    return [];
  return ["--target", target];
}

/**
 * The skia-bindings build in cargo's JSON `messages`: the directory its build
 * script ran in, and the features cargo built its library with. Throws where cargo
 * built none, or more than one, since then there is no single build to pack.
 */
export function bindingsBuildOf(messages) {
  const isBindings = (m) => /[/#]skia-bindings[@#]/.test(m.package_id ?? "");
  const scripts = messages.filter(
    (m) => m.reason === "build-script-executed" && isBindings(m),
  );
  // The library, not the build script, which cargo reports as an artifact
  // of the same package with kind `custom-build`.
  const libraries = messages.filter(
    (m) =>
      m.reason === "compiler-artifact" &&
      isBindings(m) &&
      (m.target?.kind ?? []).includes("lib"),
  );
  if (scripts.length !== 1 || libraries.length !== 1)
    throw new Error(
      `skia-pack: cargo ran ${scripts.length} skia-bindings build scripts and built ${libraries.length} libraries; one of each is packed`,
    );
  return {
    buildDir: dirname(scripts[0].out_dir),
    features: libraries[0].features,
  };
}

/// The manifest `build.rs` reads, at the crate's root and in its package.
export const MANIFEST = "skia-binaries.txt";

/// Each npm target in `lib/targets.json` as the Rust triple a crate build
/// names, and the platform whose patches its Skia carries (`PATCHES` in
/// scripts/skia-source.mjs). The archives are hosted for exactly these, so
/// a crate build reaches what an npm install does.
export const HOSTED = {
  "darwin-arm64": ["aarch64-apple-darwin", "darwin"],
  "linux-x64-glibc": ["x86_64-unknown-linux-gnu", "linux"],
  "linux-x64-musl": ["x86_64-unknown-linux-musl", "linux"],
  "linux-arm64-glibc": ["aarch64-unknown-linux-gnu", "linux"],
  "linux-arm64-musl": ["aarch64-unknown-linux-musl", "linux"],
  "win32-x64": ["x86_64-pc-windows-msvc", "win32"],
  "win32-arm64": ["aarch64-pc-windows-msvc", "win32"],
};

/// The crate's feature sets each platform's archives are built for: every
/// combination of the features that change skia-bindings' build there --
/// `metal` on macOS, `vulkan` on Linux and Windows. The crate's other
/// features reach no skia-bindings feature: `freetype` is empty, and Linux
/// builds embed FreeType whatever is set (Cargo.toml), which `cargo tree`
/// resolves into every Linux key.
export const CRATE_FEATURE_SETS = {
  darwin: [[], ["metal"]],
  linux: [[], ["vulkan"]],
  win32: [[], ["vulkan"]],
};

/**
 * The C runtimes a consumer of `target` builds with, each its own key
 * (`crtStaticIn`): both on Windows, where MSVC links it dynamically unless
 * asked, and on musl, where Rust links it statically unless asked -- the npm
 * build is the one that asks there. One otherwise, the target's default.
 */
export function crtVariants(target) {
  return target.includes("-windows-") || target.endsWith("-musl")
    ? [false, true]
    : [false];
}

/**
 * Every archive this checkout publishes: for each hosted target, crate feature
 * set and C runtime, the key skia-bindings will ask for. `resolve(target,
 * features)` gives skia-bindings' features for the crate's `features` on
 * `target`, as cargo resolves them.
 */
export function planCells(commit, resolve) {
  return Object.values(HOSTED).flatMap(([target, platform]) =>
    CRATE_FEATURE_SETS[platform].flatMap((features) => {
      const resolved = resolve(target, features);
      if (!resolved)
        throw new Error(
          `skia-pack: cargo resolved no skia-bindings features for ${target} [${features}]`,
        );
      return crtVariants(target).map((crtStatic) => ({
        target,
        platform,
        features: features.join(","),
        crtStatic,
        key: key(commit, target, resolved, crtStatic),
      }));
    }),
  );
}

/**
 * What uploading an archive whose bytes hash to `digest`, `sha256:<hex>`,
 * does to a release that holds `held` under its name -- the asset's own
 * `digest`, or `undefined` for none: `"upload"`, `"held"` for the same
 * bytes, which a re-run after a partial upload meets, or the refusal.
 * Archives are never replaced: the URL a consumer sets carries no hash
 * their build could check, so a replaced archive would change what an
 * existing build links with nothing to show for it.
 */
export function uploadDecision(name, digest, held) {
  if (held === undefined) return "upload";
  if (held === digest) return "held";
  return `skia-pack: the release already holds ${name} as ${held}; this build is ${digest}, and archives are never replaced`;
}

/// What skia-bindings prints when its download installed the archive, and
/// when it fell back to building Skia instead (binary_cache/download.rs and
/// build.rs).
const DOWNLOADED = "DOWNLOAD AND INSTALL SUCCEEDED";
const NOT_DOWNLOADED = ["DOWNLOAD AND INSTALL FAILED", "STARTING A FULL BUILD"];

/**
 * Why a crate build did not link what `mode` asks for, or `null`.
 * `bindingsLogs` are skia-bindings' build-script outputs, `crateLogs` this
 * crate's. A `patched` build downloaded and turned on `meo_skia_marker`,
 * which only skia-binaries.txt's cfgs give a downloaded build, so the
 * archive it linked was this repository's. A `stock` build downloaded too
 * -- crates.io's Skia, as 0.17.1 did -- and turned on no patch cfg.
 */
export function consumerRefusal(bindingsLogs, crateLogs, mode) {
  if (!bindingsLogs.length) return "no skia-bindings build ran";
  if (!crateLogs.length) return "no meo-skia-canvas build script ran";
  const fell = NOT_DOWNLOADED.find((line) =>
    bindingsLogs.some((log) => log.includes(line)),
  );
  if (fell) return `skia-bindings printed "${fell}"`;
  if (!bindingsLogs.some((log) => log.includes(DOWNLOADED)))
    return `skia-bindings never printed "${DOWNLOADED}"`;
  const cfgs = crateLogs.flatMap((log) =>
    [...log.matchAll(/^cargo:rustc-cfg=(meo_\w+)$/gm)].map((m) => m[1]),
  );
  if (mode === "patched" && !cfgs.includes("meo_skia_marker"))
    return "the build turned on no meo_skia_marker, so it did not take the archives' cfgs";
  if (mode === "stock" && cfgs.length)
    return `a stock build turned on ${cfgs.join(", ")}`;
  return null;
}

/** The archive file names `cells` plan that `held` does not include. */
export function missingArchives(cells, held) {
  return cells
    .map((cell) => archiveName(cell.key))
    .filter((name) => !held.includes(name));
}

/** The archive file name for `key`. */
export const archiveName = (name) => `${ARCHIVE_NAME}-${name}.tar.gz`;

/**
 * What `pack` made, for a build's log: the archive, then each file it holds
 * with its size, so a log shows an archive missing its libraries.
 */
export const packedReport = ({ archive, files }) =>
  [
    `skia-pack: packed ${archive}`,
    ...files.map(({ name, bytes }) => `  ${name} ${bytes} bytes`),
  ].join("\n");

/// Hex digits of the digest in a release's name, as many as a tree's
/// (`PATCH_DIGEST_LENGTH` in scripts/skia-source.mjs).
const RELEASE_DIGEST_LENGTH = 12;

/**
 * The release the archives of a patch set are uploaded under, and so part of
 * the URL a consumer sets: `skia-binaries-<version>-<digest>`, the digest
 * over every hosted platform's tree. A tree names one platform's patches
 * (`treeName`), and the URL has to change when any platform's do, so no one
 * tree can name the release.
 */
export function releaseName(version, trees) {
  const digest = createHash("sha256")
    .update(trees.join("\n"))
    .digest("hex")
    .slice(0, RELEASE_DIGEST_LENGTH);
  return `${ARCHIVE_NAME}-${version}-${digest}`;
}

/**
 * `skia-binaries.txt` for the archives of `release`: the URL under
 * `repository`, and each hosted target with the tree its archives are built
 * from, `treeFor(platform)`, and the cfgs `cfgsFor(platform)` gives its
 * platform.
 */
export function manifestText(release, repository, treeFor, cfgsFor) {
  return [
    `# Generated by \`node scripts/skia-pack.mjs manifest\` from PATCHES and`,
    `# build.rs's SKIA_PATCHES; \`just check-skia-pack\` refuses an edit by hand.`,
    `release ${release}`,
    `url ${repository}/releases/download/${release}/${ARCHIVE_NAME}-{key}.tar.gz`,
    ...Object.values(HOSTED).map(([triple, platform]) =>
      ["target", triple, treeFor(platform), ...cfgsFor(platform)].join(" "),
    ),
    "",
  ].join("\n");
}

/**
 * The `SKIA_BINARIES_URL = "..."` lines in `text` that name anything but
 * `url`, or `null` where it has none, which for a doc that shows the line is
 * as wrong as a stale one.
 */
export function staleDocLines(text, url) {
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.startsWith("SKIA_BINARIES_URL = "));
  if (!lines.length) return null;
  return lines.filter((line) => line !== `SKIA_BINARIES_URL = "${url}"`);
}

/// The docs that show a consumer the `SKIA_BINARIES_URL` line, which
/// `manifest --check` holds to the manifest's URL so a patch change cannot
/// leave them naming the last tree.
const DOCS_WITH_THE_LINE = [
  "README.md",
  "docs/readme/crate.md",
  "docs/rust.md",
];

/** The self-test's cases, each `[what, accepted, run]`. */
function cases(scratch) {
  const commit = "ce6727ef251518df37bcfdde4865b71f7e3a4352";
  const base = [
    "binary-cache",
    "embed-icudtl",
    "jpeg",
    "jpeg-decode",
    "jpeg-encode",
    "pdf",
    "svg",
    "textlayout",
    "webp",
    "webp-decode",
    "webp-encode",
  ];
  const keyIs =
    (expected, ...args) =>
    () => {
      const got = key(...args);
      if (got !== expected) throw new Error(`key ${got}, not ${expected}`);
    };
  // A build dir in the shape a skia-bindings build leaves, its `output`
  // linking `libs`, with `files` present in `out/skia`.
  // `layout` "nightly" writes the log where nightly cargo's build directory
  // keeps it, `run/stdout`, rather than stable's `output`.
  // On Windows skia-bindings names the libraries it built with a bare
  // `rustc-link-lib=`, as it does the system ones (cargo.rs
  // `add_static_link_lib`); elsewhere with `static=`.
  // `before` and `after` are link lines around the built libraries' run,
  // as a target's system libraries are; `form` overrides the run's prefix.
  const build = (
    name,
    libs,
    files,
    { layout = "stable", before = [], after, form } = {},
  ) => {
    const dir = join(scratch, name);
    mkdirSync(join(dir, "out", "skia"), { recursive: true });
    if (layout === "nightly") mkdirSync(join(dir, "run"));
    const windowsForm = files.some((file) => file.endsWith(".lib"));
    const prefix = form ?? (windowsForm ? "" : "static=");
    writeFileSync(
      join(dir, layout === "nightly" ? "run/stdout" : "output"),
      [
        "STARTING OFFLINE BUILD",
        ...before,
        ...libs.map((lib) => `cargo:rustc-link-lib=${prefix}${lib}`),
        ...(after ??
          (windowsForm
            ? ["cargo:rustc-link-lib=user32", "cargo:rustc-link-lib=opengl32"]
            : ["cargo:rustc-link-lib=c++"])),
      ].join("\n"),
    );
    for (const file of files)
      writeFileSync(join(dir, "out", "skia", file), file);
    return dir;
  };
  // What a build with `base`'s features links, in its order: the macOS
  // archive of skia-binaries-0.156.0-3f566dd7198c holds these.
  const libs = [
    "skparagraph",
    "skshaper",
    "skunicode_core",
    "skunicode_icu",
    "svg",
    "skresources",
    "skia",
    "skia-bindings",
  ];
  const unixFiles = [...libs.map((l) => `lib${l}.a`), "bindings.rs"];
  const windowsFiles = [
    ...libs.map((l) => `${l}.lib`),
    "bindings.rs",
    ICUDTL_DAT,
  ];
  // A refusal case expects the archive an acceptance would make, so that
  // only `pack` refusing, and not a listing that differs, fails it.
  const how = (target, features = base) => ({
    commit,
    target,
    features,
    crtStatic: false,
    version: "0.156.0",
  });
  const packs = (dir, target, vouch, expected) => () => {
    const out = join(scratch, `out-${dir.split("/").pop()}`);
    const { archive, files } = pack(dir, out, how(target), vouch);
    const listing = execFileSync("tar", ["-tzf", archive], { encoding: "utf8" })
      .split("\n")
      .filter((line) => line && !line.endsWith("/"))
      .map((line) => line.replace(/^\.\//, ""))
      .sort();
    // skia-bindings' `export` writes `tag.txt` and `key.txt` beside the
    // libraries (binaries.rs `prepare_export_directory`), and so does `pack`.
    const want = [...expected, "tag.txt", "key.txt"]
      .map((f) => `${ARCHIVE_NAME}/${f}`)
      .sort();
    if (JSON.stringify(listing) !== JSON.stringify(want))
      throw new Error(`archive holds ${listing}, not ${want}`);
    // What `pack` reports holding is what the archive holds, sized.
    const reported = files.map(({ name }) => `${ARCHIVE_NAME}/${name}`).sort();
    if (JSON.stringify(reported) !== JSON.stringify(want))
      throw new Error(`pack reports ${reported}, the archive holds ${want}`);

    const keyText = execFileSync(
      "tar",
      ["-xzOf", archive, `${ARCHIVE_NAME}/key.txt`],
      { encoding: "utf8" },
    );
    const name = archive.split("/").pop();
    if (name !== `${ARCHIVE_NAME}-${keyText}.tar.gz`)
      throw new Error(`key.txt holds ${keyText}, the archive is ${name}`);
    const sized = `  key.txt ${Buffer.byteLength(keyText)} bytes`;
    if (!packedReport({ archive, files }).split("\n").includes(sized))
      throw new Error(`the report has no line \`${sized}\``);
  };
  const linux = "x86_64-unknown-linux-gnu";
  const windows = "x86_64-pc-windows-msvc";
  const docUrl =
    "https://h/releases/download/skia-binaries-0.156.0-aaa/skia-binaries-{key}.tar.gz";
  const docsAre = (text, expected) => () => {
    const got = staleDocLines(text, docUrl);
    if (JSON.stringify(got) !== JSON.stringify(expected))
      throw new Error(`stale lines ${JSON.stringify(got)}`);
  };
  const trees = {
    darwin: "skia-0.156.0-918bd4ffa80d",
    linux: "skia-0.156.0-732ea61409ba",
    win32: "skia-0.156.0-c10797dfeaef",
  };
  const manifestIs = () => {
    const got = manifestText(
      "skia-binaries-0.156.0-0123456789ab",
      "https://github.com/l7aromeo/meo-skia-canvas",
      (platform) => trees[platform],
      (platform) => ({ darwin: ["a", "b"], linux: ["b"], win32: [] })[platform],
    );
    const want = [
      "# Generated by `node scripts/skia-pack.mjs manifest` from PATCHES and",
      "# build.rs's SKIA_PATCHES; `just check-skia-pack` refuses an edit by hand.",
      "release skia-binaries-0.156.0-0123456789ab",
      "url https://github.com/l7aromeo/meo-skia-canvas/releases/download/skia-binaries-0.156.0-0123456789ab/skia-binaries-{key}.tar.gz",
      "target aarch64-apple-darwin skia-0.156.0-918bd4ffa80d a b",
      "target x86_64-unknown-linux-gnu skia-0.156.0-732ea61409ba b",
      "target x86_64-unknown-linux-musl skia-0.156.0-732ea61409ba b",
      "target aarch64-unknown-linux-gnu skia-0.156.0-732ea61409ba b",
      "target aarch64-unknown-linux-musl skia-0.156.0-732ea61409ba b",
      "target x86_64-pc-windows-msvc skia-0.156.0-c10797dfeaef",
      "target aarch64-pc-windows-msvc skia-0.156.0-c10797dfeaef",
      "",
    ].join("\n");
    if (got !== want) throw new Error(`manifest:\n${got}`);
  };
  // The release follows every platform's tree, so a patch to one platform
  // alone still moves the URL.
  const releaseMoves = () => {
    const base = Object.values(trees);
    const name = releaseName("0.156.0", base);
    if (!/^skia-binaries-0\.156\.0-[0-9a-f]{12}$/.test(name))
      throw new Error(`release ${name}`);
    for (let i = 0; i < base.length; i += 1) {
      const moved = base.map((tree, j) => (j === i ? `${tree}0` : tree));
      if (releaseName("0.156.0", moved) === name)
        throw new Error(`a change to ${base[i]} alone kept ${name}`);
    }
    if (releaseName("0.157.0", base) === name)
      throw new Error("a new skia-bindings version kept the release");
  };
  const crtIs = (cfg, expected) => () => {
    if (crtStaticIn(cfg) !== expected)
      throw new Error(`crt-static read as ${!expected}`);
  };
  const muslCfg =
    'target_env="musl"\ntarget_feature="crt-static"\ntarget_os="linux"\n';
  const msvcCfg =
    'target_env="msvc"\ntarget_feature="sse2"\ntarget_os="windows"\n';
  // cargo's JSON messages for a build, skia-bindings' ids in the form cargo
  // gives a registry package and a path one.
  const message = (reason, id, extra) => ({ reason, package_id: id, ...extra });
  const registryId =
    "registry+https://github.com/rust-lang/crates.io-index#skia-bindings@0.156.0";
  const pathId = "path+file:///w/rust-skia/skia-bindings#0.156.0";
  const bindingsIs = (messages, expected) => () => {
    const got = bindingsBuildOf(messages);
    if (JSON.stringify(got) !== JSON.stringify(expected))
      throw new Error(`bindings build ${JSON.stringify(got)}`);
  };
  // Refused for the reason `bindingsBuildOf` gives, not any exception.
  const bindingsRefused = (messages) => () => {
    try {
      bindingsBuildOf(messages);
    } catch (e) {
      if (e.message.endsWith("one of each is packed")) throw e;
      return;
    }
  };
  const ran = (id, dir) =>
    message("build-script-executed", id, { out_dir: `${dir}/out` });
  // A library's artifact, and the build script's own: cargo reports the
  // compiled build script as a `compiler-artifact` of the package too.
  const built = (id, features) =>
    message("compiler-artifact", id, { features, target: { kind: ["lib"] } });
  const scriptBuilt = (id, features) =>
    message("compiler-artifact", id, {
      features,
      target: { kind: ["custom-build"] },
    });
  // A resolver in the shape cargo answers: the base set, plus what each
  // crate feature turns on.
  const resolveLike = (target, features) => [
    ...base,
    ...(features.includes("metal") ? ["ganesh", "metal"] : []),
    ...(features.includes("vulkan") ? ["ganesh", "vulkan"] : []),
    ...(target.includes("-linux-") ? ["embed-freetype", "freetype-woff2"] : []),
  ];
  const planIs = () => {
    const cells = planCells(commit, resolveLike);
    const keys = new Set(cells.map((cell) => cell.key));
    if (cells.length !== 22) throw new Error(`${cells.length} cells`);
    if (keys.size !== cells.length) throw new Error("two cells share a key");
    for (const [target] of Object.values(HOSTED))
      if (!cells.some((cell) => cell.target === target))
        throw new Error(`no cell for ${target}`);
    for (const name of [
      "ce6727ef251518df37bc-x86_64-unknown-linux-musl-ftembed-ftwoff2-jpegd-jpege-pdf-svg-textlayout-webpd-webpe-static",
      "ce6727ef251518df37bc-x86_64-unknown-linux-musl-ftembed-ftwoff2-jpegd-jpege-pdf-svg-textlayout-webpd-webpe",
      "ce6727ef251518df37bc-aarch64-pc-windows-msvc-ganesh-jpegd-jpege-pdf-svg-textlayout-vulkan-webpd-webpe-static",
      "ce6727ef251518df37bc-aarch64-apple-darwin-ganesh-jpegd-jpege-metal-pdf-svg-textlayout-webpd-webpe",
      "ce6727ef251518df37bc-aarch64-unknown-linux-gnu-ftembed-ftwoff2-ganesh-jpegd-jpege-pdf-svg-textlayout-vulkan-webpd-webpe",
    ])
      if (!keys.has(name)) throw new Error(`no cell keyed ${name}`);
    for (const cell of cells)
      if (cell.crtStatic && cell.target.endsWith("-gnu"))
        throw new Error(`a static cell for ${cell.target}`);
  };
  const missingIs = (held, expected) => () => {
    const cells = planCells(commit, resolveLike);
    const got = missingArchives(cells, held(cells));
    if (JSON.stringify(got) !== JSON.stringify(expected(cells)))
      throw new Error(`missing ${got.length}: ${got.slice(0, 3)}`);
  };
  const downloaded =
    "TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES: 0.156.0/k\nDOWNLOAD AND INSTALL SUCCEEDED\n";
  const fellBack =
    'TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES: 0.156.0/k\nDOWNLOAD AND INSTALL FAILED: curl error code: "22"\nSTARTING A FULL BUILD\n';
  const patchedCrate =
    "cargo:rustc-check-cfg=cfg(meo_skia_marker)\ncargo:rustc-cfg=meo_atlas_size\ncargo:rustc-cfg=meo_skia_marker\n";
  const stockCrate = "cargo:rustc-check-cfg=cfg(meo_skia_marker)\n";
  const consumerIs = (bindings, crate, mode, expected) => () => {
    const got = consumerRefusal(bindings, crate, mode);
    if (expected === null ? got !== null : !got?.includes(expected))
      throw new Error(`consumer refusal ${JSON.stringify(got)}`);
  };
  return [
    [
      "a patched consumer build that downloaded and took the archives' cfgs",
      true,
      consumerIs([downloaded], [patchedCrate], "patched", null),
    ],
    [
      "a stock consumer build that downloaded and took no patch cfg",
      true,
      consumerIs([downloaded, ""], [stockCrate], "stock", null),
    ],
    [
      "a consumer build whose download fell back to a full build",
      true,
      consumerIs([fellBack], [patchedCrate], "patched", "INSTALL FAILED"),
    ],
    [
      "a consumer build that never tried to download",
      true,
      consumerIs(
        ["STARTING OFFLINE BUILD\n"],
        [stockCrate],
        "stock",
        "never printed",
      ),
    ],
    [
      "a patched consumer build without the marker cfg",
      true,
      consumerIs([downloaded], [stockCrate], "patched", "meo_skia_marker"),
    ],
    [
      "a stock consumer build with a patch cfg",
      true,
      consumerIs([downloaded], [patchedCrate], "stock", "meo_atlas_size"),
    ],
    [
      "a consumer target with no skia-bindings build",
      true,
      consumerIs([], [stockCrate], "stock", "no skia-bindings"),
    ],
    [
      "22 archives: every hosted target, feature set and consumer C runtime",
      true,
      planIs,
    ],
    [
      "a plan with a feature set cargo cannot resolve",
      false,
      () => {
        try {
          planCells(commit, () => null);
        } catch (e) {
          if (e.message.includes("resolved no skia-bindings features")) throw e;
        }
      },
    ],
    [
      "an archive the release does not hold is uploaded",
      true,
      () => {
        if (uploadDecision("a.tar.gz", "sha256:aa", undefined) !== "upload")
          throw new Error("not uploaded");
      },
    ],
    [
      "an archive the release holds with the same bytes is left as it is",
      true,
      () => {
        if (uploadDecision("a.tar.gz", "sha256:aa", "sha256:aa") !== "held")
          throw new Error("not held");
      },
    ],
    [
      "an archive the release holds with other bytes is refused",
      false,
      () => {
        const why = uploadDecision("a.tar.gz", "sha256:aa", "sha256:bb");
        if (why.includes("never replaced")) throw new Error(why);
      },
    ],
    [
      "a release holding every planned archive, and more",
      true,
      missingIs(
        (cells) => [
          "skia-binaries-other.tar.gz",
          ...cells.map((c) => archiveName(c.key)),
        ],
        () => [],
      ),
    ],
    [
      "a release missing one archive names it",
      true,
      missingIs(
        (cells) => cells.slice(1).map((c) => archiveName(c.key)),
        (cells) => [archiveName(cells[0].key)],
      ),
    ],
    [
      "a build for its host passes no --target, one for another target does",
      true,
      () => {
        const got = [
          targetArgs(
            "x86_64-unknown-linux-musl",
            "x86_64-unknown-linux-musl",
            [],
          ),
          targetArgs("aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc", []),
          targetArgs("x86_64-unknown-linux-musl", "x86_64-unknown-linux-musl", [
            "--target",
            "x86_64-unknown-linux-musl",
          ]),
          targetArgs("aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu", [
            "--target=aarch64-unknown-linux-gnu",
          ]),
        ];
        const want = [[], ["--target", "aarch64-pc-windows-msvc"], [], []];
        if (JSON.stringify(got) !== JSON.stringify(want))
          throw new Error(JSON.stringify(got));
      },
    ],
    ["a musl target's default static C runtime", true, crtIs(muslCfg, true)],
    ["MSVC's dynamic C runtime", true, crtIs(msvcCfg, false)],
    [
      "crt-static is not read from a longer feature name",
      true,
      crtIs('target_feature="crt-static-x"\n', false),
    ],
    [
      "the registry skia-bindings build and its features",
      true,
      bindingsIs(
        [
          scriptBuilt(registryId, ["pdf", "svg"]),
          ran(registryId, "/t/release/build/skia-bindings-1"),
          ran("registry+https://x#other@1.0.0", "/t/release/build/other-1"),
          built(registryId, ["pdf", "svg"]),
        ],
        {
          buildDir: "/t/release/build/skia-bindings-1",
          features: ["pdf", "svg"],
        },
      ),
    ],
    [
      "a path skia-bindings, as under use-local-skia",
      true,
      bindingsIs(
        [ran(pathId, "/t/b/skia-bindings-2"), built(pathId, ["metal"])],
        { buildDir: "/t/b/skia-bindings-2", features: ["metal"] },
      ),
    ],
    [
      "a build that ran no skia-bindings build script",
      false,
      bindingsRefused([built(registryId, [])]),
    ],
    [
      "a build that ran skia-bindings' build script twice",
      false,
      bindingsRefused([
        ran(registryId, "/a/skia-bindings-1"),
        ran(registryId, "/a/skia-bindings-2"),
        built(registryId, []),
      ]),
    ],
    [
      "the manifest names the release, the URL and every hosted target's tree",
      true,
      manifestIs,
    ],
    ["the release moves with any one platform's tree", true, releaseMoves],
    [
      "a doc whose line names the manifest's URL",
      true,
      docsAre(`[env]\nSKIA_BINARIES_URL = "${docUrl}"\n`, []),
    ],
    [
      "a doc whose line names another tree",
      true,
      docsAre(`SKIA_BINARIES_URL = "${docUrl.replace("aaa", "bbb")}"\n`, [
        `SKIA_BINARIES_URL = "${docUrl.replace("aaa", "bbb")}"`,
      ]),
    ],
    ["a doc with no line at all", true, docsAre("nothing here\n", null)],
    [
      "the base set's key on Linux",
      true,
      keyIs(
        "ce6727ef251518df37bc-x86_64-unknown-linux-gnu-jpegd-jpege-pdf-svg-textlayout-webpd-webpe",
        commit,
        linux,
        base,
      ),
    ],
    [
      "the Metal set's key",
      true,
      keyIs(
        "ce6727ef251518df37bc-aarch64-apple-darwin-ganesh-jpegd-jpege-metal-pdf-svg-textlayout-webpd-webpe",
        commit,
        "aarch64-apple-darwin",
        [...base, "ganesh", "metal"],
      ),
    ],
    [
      "the Vulkan and FreeType set's key, sorted after the short names",
      true,
      keyIs(
        "ce6727ef251518df37bc-aarch64-unknown-linux-musl-ftembed-ftwoff2-ganesh-jpegd-jpege-pdf-svg-textlayout-vulkan-webpd-webpe",
        commit,
        "aarch64-unknown-linux-musl",
        [...base, "embed-freetype", "freetype-woff2", "ganesh", "vulkan"],
      ),
    ],
    [
      // skia-bindings drops FreeType's features on a target that does not use
      // FreeType (platform.rs `redundant_features`): this is the key its own
      // build printed for `metal,freetype` on aarch64-apple-darwin.
      "macOS drops FreeType's features from the key",
      true,
      keyIs(
        "ce6727ef251518df37bc-aarch64-apple-darwin-ganesh-jpegd-jpege-metal-pdf-svg-textlayout-webpd-webpe",
        commit,
        "aarch64-apple-darwin",
        [...base, "embed-freetype", "freetype-woff2", "ganesh", "metal"],
      ),
    ],
    [
      "Windows drops FreeType's features from the key",
      true,
      keyIs(
        "ce6727ef251518df37bc-aarch64-pc-windows-msvc-jpegd-jpege-pdf-svg-textlayout-webpd-webpe",
        commit,
        "aarch64-pc-windows-msvc",
        [...base, "embed-freetype", "freetype-woff2"],
      ),
    ],
    [
      "a static-CRT Windows key",
      true,
      keyIs(
        "ce6727ef251518df37bc-x86_64-pc-windows-msvc-jpegd-jpege-pdf-svg-textlayout-webpd-webpe-static",
        commit,
        windows,
        base,
        true,
      ),
    ],
    [
      "a Linux build packs its linked libraries and bindings.rs",
      true,
      packs(build("linux", libs, unixFiles), linux, () => null, unixFiles),
    ],
    [
      "a build in nightly cargo's directory layout packs the same",
      true,
      packs(
        build("nightly", libs, unixFiles, { layout: "nightly" }),
        "x86_64-unknown-linux-musl",
        () => null,
        unixFiles,
      ),
    ],
    [
      "a Windows build packs .lib files and icudtl.dat",
      true,
      packs(
        build("windows", libs, [
          ...libs.map((l) => `${l}.lib`),
          "bindings.rs",
          ICUDTL_DAT,
        ]),
        windows,
        () => null,
        [...libs.map((l) => `${l}.lib`), "bindings.rs", ICUDTL_DAT],
      ),
    ],
    [
      "a build check-patched-skia refuses",
      false,
      packs(
        build("unpatched", libs, unixFiles),
        linux,
        () => "unpatched",
        unixFiles,
      ),
    ],
    [
      "a build missing a library it links",
      false,
      packs(
        build(
          "missing",
          libs,
          unixFiles.filter((f) => f !== "libsvg.a"),
        ),
        linux,
        () => null,
        unixFiles,
      ),
    ],
    [
      "a Windows build missing icudtl.dat",
      false,
      packs(
        build("noicu", libs, [...libs.map((l) => `${l}.lib`), "bindings.rs"]),
        windows,
        () => null,
        windowsFiles,
      ),
    ],
    [
      "a Windows build whose output links none of its libraries",
      false,
      packs(
        build(
          "winnone",
          [],
          [...libs.map((l) => `${l}.lib`), "bindings.rs", ICUDTL_DAT],
        ),
        windows,
        () => null,
        windowsFiles,
      ),
    ],
    [
      "a build whose output leaves out a library its features build",
      false,
      packs(
        build(
          "unlinked",
          libs.filter((l) => l !== "skresources"),
          unixFiles,
        ),
        linux,
        () => null,
        unixFiles,
      ),
    ],
    [
      // Run 42517456551's musl cells: fontconfig's pkg-config probe
      // names expat before the built libraries, and
      // `link_libraries` again after them, and Skia's own build leaves a
      // libexpat.a in out/skia.
      "a musl build linking expat around its run packs without it",
      true,
      packs(
        build("musl", libs, [...unixFiles, "libexpat.a"], {
          before: ["cargo:rustc-link-lib=static=expat"],
          after: [
            "cargo:rustc-link-lib=stdc++",
            "cargo:rustc-link-lib=static=fontconfig",
            "cargo:rustc-link-lib=static=expat",
          ],
        }),
        "x86_64-unknown-linux-musl",
        () => null,
        unixFiles,
      ),
    ],
    [
      "a build linking a library its features do not build inside its run",
      false,
      packs(
        build(
          "extra",
          [...libs.slice(0, 5), "sksg", ...libs.slice(5)],
          [...unixFiles, "libsksg.a"],
        ),
        linux,
        () => null,
        unixFiles,
      ),
    ],
    [
      "a Windows build linking a library its features do not build inside its run",
      false,
      packs(
        build(
          "winextra",
          [...libs.slice(0, 5), "sksg", ...libs.slice(5)],
          [
            ...[...libs, "sksg"].map((l) => `${l}.lib`),
            "bindings.rs",
            ICUDTL_DAT,
          ],
        ),
        windows,
        () => null,
        windowsFiles,
      ),
    ],
    [
      "a Linux build naming its libraries without static=",
      false,
      packs(
        build("bare", libs, unixFiles, { form: "" }),
        linux,
        () => null,
        unixFiles,
      ),
    ],
    [
      "a Windows build naming its libraries with static=",
      false,
      packs(
        build("winstatic", libs, windowsFiles, { form: "static=" }),
        windows,
        () => null,
        windowsFiles,
      ),
    ],
    [
      "skottie builds skresources once, with or without svg",
      true,
      () => {
        const got = [
          builtLibraries(["skottie"]).join(),
          builtLibraries(["svg", "skottie"]).join(),
        ];
        const want = [
          "skottie,sksg,jsonreader,skresources,skia,skia-bindings",
          "svg,skresources,skottie,sksg,jsonreader,skia,skia-bindings",
        ];
        if (JSON.stringify(got) !== JSON.stringify(want))
          throw new Error(`${got.join(" / ")}`);
      },
    ],
  ];
}

function selfTest() {
  const scratch = mkdtempSync(join(tmpdir(), "skia-pack-"));
  let failed = 0;
  try {
    for (const [what, accepted, run] of cases(scratch)) {
      let error = null;
      try {
        run();
      } catch (e) {
        error = e;
      }
      if ((error === null) !== accepted) {
        console.error(
          `skia-pack self-test: ${what} was ${accepted ? `refused: ${error.message}` : "accepted"}`,
        );
        failed += 1;
      }
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  if (failed) process.exit(1);
  console.log(
    `skia-pack: self-test, ${cases(scratch).length} cases, the keys skia-bindings computes and the archives it unpacks`,
  );
}

/** The platform `PATCHES` names for `target`'s patches. */
export function platformOf(target) {
  if (target.includes("-apple-")) return "darwin";
  if (target.includes("-windows-")) return "win32";
  return "linux";
}

/**
 * The skia-bindings version Cargo.lock resolves, and the rust-skia commit its
 * crates.io package was made from, read from that package's
 * `.cargo_vcs_info.json` in the registry: the commit a crate build keys its
 * download by (binary_cache/download.rs `should_try_download_binaries`).
 */
function packagedSkiaBindings() {
  const lock = readFileSync(join(ROOT, "Cargo.lock"), "utf8");
  const version = lock.match(
    /^name = "skia-bindings"\nversion = "([^"]+)"$/m,
  )?.[1];
  if (!version) throw new Error("skia-pack: Cargo.lock names no skia-bindings");
  const registry = join(
    process.env.CARGO_HOME ?? join(process.env.HOME ?? "", ".cargo"),
    "registry",
    "src",
  );
  const infos = (existsSync(registry) ? readdirSync(registry) : [])
    .map((index) =>
      join(registry, index, `skia-bindings-${version}`, ".cargo_vcs_info.json"),
    )
    .filter(existsSync);
  if (infos.length !== 1)
    throw new Error(
      `skia-pack: ${infos.length} registry copies of skia-bindings ${version} carry .cargo_vcs_info.json`,
    );
  const commit = JSON.parse(readFileSync(infos[0], "utf8")).git?.sha1;
  if (!/^[0-9a-f]{40}$/.test(commit ?? ""))
    throw new Error(`skia-pack: ${infos[0]} names no commit`);
  return { commit, version };
}

/**
 * Packs the skia-bindings build in `buildDir` for `target`, once
 * scripts/check-patched-skia.mjs finds it compiled `target`'s patched files
 * and downloaded nothing. Returns what `pack` does.
 */
async function packChecked(buildDir, outDir, target, features, crtStatic) {
  const { commit, version } = packagedSkiaBindings();
  const { PATCHED, buildLog, refusal } =
    await import("./check-patched-skia.mjs");
  const patched = PATCHED[platformOf(target)];
  return pack(
    buildDir,
    outDir,
    { commit, target, features, crtStatic, version },
    () => refusal(buildLog(buildDir), patched),
  );
}

/**
 * The flags cargo passes rustc for the target, from the environment, in
 * cargo's own order of precedence: `CARGO_ENCODED_RUSTFLAGS`, split on the
 * unit separator, else `RUSTFLAGS`, split on whitespace.
 */
function rustflags() {
  const encoded = process.env.CARGO_ENCODED_RUSTFLAGS;
  if (encoded !== undefined) return encoded ? encoded.split("\x1f") : [];
  return (process.env.RUSTFLAGS ?? "").split(/\s+/).filter(Boolean);
}

/**
 * The asset names of the release `tag` in `repository`, or none where there
 * is no such release yet: before the first upload for a tree, every archive
 * is missing, which is the same answer.
 */
function releaseAssets(tag, repository) {
  try {
    return execFileSync(
      "gh",
      [
        "release",
        "view",
        tag,
        "--repo",
        repository,
        "--json",
        "assets",
        "--jq",
        ".assets[].name",
      ],
      { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    )
      .split("\n")
      .filter(Boolean);
  } catch (error) {
    if (String(error.stderr).includes("release not found")) return [];
    throw error;
  }
}

/**
 * Uploads `archive` to the release `tag` of `repository` (`owner/name`)
 * through GitHub's REST API with `token`, as `uploadDecision` allows.
 * Plain `fetch` rather than `gh` or an action: it runs in every cell,
 * the arm64 Alpine container included, where no JavaScript action does.
 */
async function uploadArchive(archive, tag, repository, token) {
  const headers = {
    Authorization: `Bearer ${token}`,
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  const found = await fetch(
    `https://api.github.com/repos/${repository}/releases/tags/${tag}`,
    { headers },
  );
  if (!found.ok)
    throw new Error(
      `skia-pack: no release ${tag} in ${repository}: ${found.status}`,
    );
  const release = await found.json();
  const name = archive.split(/[\\/]/).at(-1);
  const bytes = readFileSync(archive);
  const digest = `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
  const decision = uploadDecision(
    name,
    digest,
    release.assets.find((asset) => asset.name === name)?.digest,
  );
  if (decision === "held") return `${name}: already held, same bytes`;
  if (decision !== "upload") throw new Error(decision);
  const sent = await fetch(
    `https://uploads.github.com/repos/${repository}/releases/${release.id}/assets?name=${encodeURIComponent(name)}`,
    {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/gzip" },
      body: bytes,
    },
  );
  if (!sent.ok)
    throw new Error(
      `skia-pack: uploading ${name} to ${tag} failed: ${sent.status} ${await sent.text()}`,
    );
  return `${name}: uploaded, ${digest}`;
}

/**
 * This tree's `planCells`, keyed by the commit crates.io's skia-bindings was
 * packaged from, its features as cargo resolves them for each target.
 */
async function currentPlan() {
  const { commit } = packagedSkiaBindings();
  const { skiaFeatures } = await import("./skia-binaries.mjs");
  return planCells(commit, (target, features) =>
    skiaFeatures([
      "--target",
      target,
      ...(features.length ? ["--features", features.join(",")] : []),
    ]),
  );
}

/**
 * This checkout's `skia-binaries.txt`: for each platform the tree `PATCHES`
 * names for it and the cfgs of build.rs's `SKIA_PATCHES` whose file and
 * marker that platform's check (`PATCHED` in scripts/check-patched-skia.mjs)
 * holds a build to, and the release all of the trees name together. The
 * same on every host: nothing here reads the platform it runs on.
 */
async function currentManifest() {
  const { PATCHES, treeName } = await import("./skia-source.mjs");
  const { PATCHED, skiaPatchTable } = await import("./check-patched-skia.mjs");
  const table = skiaPatchTable(readFileSync(join(ROOT, "build.rs"), "utf8"));
  if (table.error) throw new Error(`skia-pack: build.rs: ${table.error}`);
  const repository = readFileSync(join(ROOT, "Cargo.toml"), "utf8").match(
    /^repository = "([^"]+)"$/m,
  )?.[1];
  if (!repository) throw new Error("skia-pack: Cargo.toml names no repository");
  const cfgsFor = (platform) =>
    Object.entries(table.entries)
      .filter(([, { file, marker }]) =>
        PATCHED[platform].some((p) => p.file === file && p.marker === marker),
      )
      .map(([cfg]) => cfg);
  const platforms = [
    ...new Set(Object.values(HOSTED).map(([, platform]) => platform)),
  ];
  const trees = Object.fromEntries(
    platforms.map((platform) => [
      platform,
      treeName(ROOT, PATCHES[platform] ?? []),
    ]),
  );
  const version = trees[platforms[0]].match(/^skia-(.+)-[0-9a-f]+$/)?.[1];
  if (!version)
    throw new Error(`skia-pack: ${trees[platforms[0]]} names no version`);
  return manifestText(
    releaseName(version, Object.values(trees)),
    repository,
    (platform) => trees[platform],
    cfgsFor,
  );
}

const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (main) {
  const [command, ...args] = process.argv.slice(2);
  try {
    if (command === "--self-test") selfTest();
    else if (command === "manifest") {
      const text = await currentManifest();
      const path = join(ROOT, MANIFEST);
      if (args[0] === "--check") {
        const held = existsSync(path) ? readFileSync(path, "utf8") : "";
        if (held !== text) {
          console.error(
            `skia-pack: ${MANIFEST} is not what PATCHES and build.rs give; run \`node scripts/skia-pack.mjs manifest\``,
          );
          process.exit(1);
        }
        const url = text.match(/^url (\S+)$/m)[1];
        const docs = DOCS_WITH_THE_LINE.map((doc) => [
          doc,
          staleDocLines(readFileSync(join(ROOT, doc), "utf8"), url),
        ]).filter(([, stale]) => stale === null || stale.length);
        for (const [doc, stale] of docs)
          console.error(
            `skia-pack: ${doc} ${stale === null ? "shows no SKIA_BINARIES_URL line" : `names another URL: ${stale.join(" ")}`}; it should be\n  SKIA_BINARIES_URL = "${url}"`,
          );
        if (docs.length) process.exit(1);
        console.log(
          `skia-pack: ${MANIFEST} matches PATCHES and build.rs, and ${DOCS_WITH_THE_LINE.length} docs show its URL`,
        );
      } else {
        writeFileSync(path, text);
        console.log(`skia-pack: wrote ${MANIFEST}`);
      }
    } else if (command === "pack") {
      const [buildDir, outDir, target, features, crtStatic] = args;
      if (!buildDir || !outDir || !target || !features)
        throw new Error(
          "usage: node scripts/skia-pack.mjs pack <build-dir> <out-dir> <target> <features> [static]",
        );
      const packed = await packChecked(
        buildDir,
        outDir,
        target,
        features.split(","),
        crtStatic === "static",
      );
      console.log(packedReport(packed));
    } else if (command === "upload") {
      const [archive, tag] = args;
      const repository = process.env.GITHUB_REPOSITORY;
      const token = process.env.GITHUB_TOKEN;
      if (!archive || !tag || !repository || !token)
        throw new Error(
          "usage: GITHUB_REPOSITORY=<owner/name> GITHUB_TOKEN=<token> node scripts/skia-pack.mjs upload <archive> <release tag>",
        );
      console.log(await uploadArchive(archive, tag, repository, token));
    } else if (command === "consumer") {
      const [targetDir, mode] = args;
      if (!targetDir || !["patched", "stock"].includes(mode))
        throw new Error(
          "usage: node scripts/skia-pack.mjs consumer <target-dir> patched|stock",
        );
      const logs = (prefix) =>
        readdirSync(targetDir, { recursive: true })
          .map(String)
          .filter((path) => {
            const parts = path.split(/[\\/]/);
            return (
              parts.at(-1) === "output" &&
              parts.at(-3) === "build" &&
              parts.at(-2).startsWith(prefix)
            );
          })
          .map((path) => readFileSync(join(targetDir, path), "utf8"));
      const why = consumerRefusal(
        logs("skia-bindings-"),
        logs("meo-skia-canvas-"),
        mode,
      );
      if (why) throw new Error(`skia-pack: ${mode} crate build: ${why}`);
      console.log(
        `skia-pack: the ${mode} crate build linked what it asked for`,
      );
    } else if (command === "origin") {
      const { version, commit } = packagedSkiaBindings();
      console.log(`${version} ${commit}`);
    } else if (command === "plan") {
      const cells = await currentPlan();
      console.log(JSON.stringify(cells));
    } else if (command === "published") {
      const cells = await currentPlan();
      const manifest = readFileSync(join(ROOT, MANIFEST), "utf8");
      const tag = manifest.match(/^release (\S+)$/m)?.[1];
      const repository = manifest
        .match(/^url (\S+)$/m)?.[1]
        ?.split("/releases/download/")[0];
      if (!tag || !repository)
        throw new Error(`skia-pack: ${MANIFEST} names no release or URL`);
      const held = releaseAssets(tag, repository);
      const missing = missingArchives(cells, held);
      if (missing.length) {
        console.error(
          `skia-pack: ${tag} lacks ${missing.length} of ${cells.length} archives:\n  ${missing.join("\n  ")}\nDispatch skia-binaries.yml with publish on for these patches.`,
        );
        process.exit(1);
      }
      console.log(`skia-pack: ${tag} holds all ${cells.length} archives`);
    } else if (command === "build") {
      const expectAt = args[0] === "--expect" ? 2 : 0;
      const expected = expectAt ? args[1] : null;
      const [outDir, target, ...cargoArgs] = args.slice(expectAt);
      if (!outDir || !target || (expectAt && !expected))
        throw new Error(
          "usage: node scripts/skia-pack.mjs build [--expect <key>] <out-dir> <target> [cargo args]",
        );
      const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
        /^host: (\S+)$/m,
      )?.[1];
      const json = execFileSync(
        "cargo",
        [
          "build",
          "--release",
          "--lib",
          ...targetArgs(target, host, cargoArgs),
          "--message-format=json-render-diagnostics",
          ...cargoArgs,
        ],
        {
          cwd: ROOT,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "inherit"],
          maxBuffer: 1 << 30,
        },
      );
      const { buildDir, features } = bindingsBuildOf(
        json
          .split("\n")
          .filter((line) => line.startsWith("{"))
          .map((line) => JSON.parse(line)),
      );
      // RUSTFLAGS reach the target with or without `--target`, and with
      // `-Zhost-config` they reach it alone, so they say what skia-bindings
      // read from CARGO_CFG_TARGET_FEATURE.
      const cfg = execFileSync(
        "rustc",
        ["--print", "cfg", "--target", target, ...rustflags()],
        { encoding: "utf8" },
      );
      const crtStatic = crtStaticIn(cfg);
      const { commit } = packagedSkiaBindings();
      const built = key(commit, target, features, crtStatic);
      if (expected && built !== expected)
        throw new Error(
          `skia-pack: this build packs as ${built}, not the planned ${expected}; plan and build disagree on its features or C runtime`,
        );
      const packed = await packChecked(
        buildDir,
        outDir,
        target,
        features,
        crtStatic,
      );
      console.log(packedReport(packed));
    } else {
      console.error(
        "usage: node scripts/skia-pack.mjs pack <build-dir> <out-dir> <target> <features> [static] | build [--expect <key>] <out-dir> <target> [cargo args] | plan | published | upload <archive> <release tag> | consumer <target-dir> patched|stock | origin | manifest [--check] | --self-test",
      );
      process.exit(2);
    }
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
