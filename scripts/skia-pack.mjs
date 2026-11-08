#!/usr/bin/env node
// Archives a patched Skia build in the layout skia-bindings downloads, so a
// crate built from crates.io can link this repository's Skia by pointing
// `SKIA_BINARIES_URL` at the archives.
//
//   node scripts/skia-pack.mjs pack <build-dir> <out-dir> <target> <features> [static]
//       pack a skia-bindings build that already ran: <features> are
//       skia-bindings' cargo features, comma-separated
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
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
 * The files the archive of a build carries, in `out/skia`: the static
 * libraries its `output` links, `bindings.rs`, and `icudtl.dat` on Windows
 * with `textlayout`.
 */
export function archiveFiles(output, target, features) {
  const libraries = [
    ...output.matchAll(/^cargo:rustc-link-lib=static=(\S+)$/gm),
  ].map((m) => libraryFile(target, m[1]));
  const extra =
    isWindows(target) && features.includes("textlayout") ? [ICUDTL_DAT] : [];
  return [...libraries, "bindings.rs", ...extra];
}

/**
 * Packs the skia-bindings build in `buildDir` into `outDir` as
 * `skia-binaries-<key>.tar.gz`, once `vouch` finds nothing wrong with its
 * log. `how` is `{ commit, target, features, crtStatic, version }`. Returns
 * the archive's path.
 */
export function pack(buildDir, outDir, how, vouch) {
  const output = readFileSync(join(buildDir, "output"), "utf8");
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
    mkdirSync(outDir, { recursive: true });
    const archive = join(outDir, `${ARCHIVE_NAME}-${name}.tar.gz`);
    execFileSync("tar", ["-czf", archive, "-C", staging, ARCHIVE_NAME]);
    return archive;
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
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

/**
 * The release tag the archives of `tree` are uploaded under, and so part of
 * the URL a consumer sets.
 */
export const releaseTag = (tree) =>
  `skia-binaries-${tree.replace(/^skia-/, "")}`;

/**
 * `skia-binaries.txt` for `tree`: the URL under `repository`, and each hosted
 * target with the cfgs `cfgsFor(platform)` gives its platform.
 */
export function manifestText(tree, repository, cfgsFor) {
  return [
    `# Generated by \`node scripts/skia-pack.mjs manifest\` from PATCHES and`,
    `# build.rs's SKIA_PATCHES; \`just check-skia-pack\` refuses an edit by hand.`,
    `tree ${tree}`,
    `url ${repository}/releases/download/${releaseTag(tree)}/${ARCHIVE_NAME}-{key}.tar.gz`,
    ...Object.values(HOSTED).map(([triple, platform]) =>
      ["target", triple, ...cfgsFor(platform)].join(" "),
    ),
    "",
  ].join("\n");
}

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
  const build = (name, libs, files) => {
    const dir = join(scratch, name);
    mkdirSync(join(dir, "out", "skia"), { recursive: true });
    writeFileSync(
      join(dir, "output"),
      [
        "STARTING OFFLINE BUILD",
        ...libs.map((lib) => `cargo:rustc-link-lib=static=${lib}`),
        "cargo:rustc-link-lib=c++",
      ].join("\n"),
    );
    for (const file of files)
      writeFileSync(join(dir, "out", "skia", file), file);
    return dir;
  };
  const libs = ["skparagraph", "svg", "skia", "skia-bindings"];
  const unixFiles = [...libs.map((l) => `lib${l}.a`), "bindings.rs"];
  const how = (target, features = base) => ({
    commit,
    target,
    features,
    crtStatic: false,
    version: "0.156.0",
  });
  const packs = (dir, target, vouch, expected) => () => {
    const out = join(scratch, `out-${dir.split("/").pop()}`);
    const archive = pack(dir, out, how(target), vouch);
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
    const keyText = execFileSync(
      "tar",
      ["-xzOf", archive, `${ARCHIVE_NAME}/key.txt`],
      { encoding: "utf8" },
    );
    const name = archive.split("/").pop();
    if (name !== `${ARCHIVE_NAME}-${keyText}.tar.gz`)
      throw new Error(`key.txt holds ${keyText}, the archive is ${name}`);
  };
  const linux = "x86_64-unknown-linux-gnu";
  const windows = "x86_64-pc-windows-msvc";
  const manifestIs = () => {
    const got = manifestText(
      "skia-0.156.0-3e1169e2915c",
      "https://github.com/l7aromeo/meo-skia-canvas",
      (platform) => ({ darwin: ["a", "b"], linux: ["b"], win32: [] })[platform],
    );
    const want = [
      "# Generated by `node scripts/skia-pack.mjs manifest` from PATCHES and",
      "# build.rs's SKIA_PATCHES; `just check-skia-pack` refuses an edit by hand.",
      "tree skia-0.156.0-3e1169e2915c",
      "url https://github.com/l7aromeo/meo-skia-canvas/releases/download/skia-binaries-0.156.0-3e1169e2915c/skia-binaries-{key}.tar.gz",
      "target aarch64-apple-darwin a b",
      "target x86_64-unknown-linux-gnu b",
      "target x86_64-unknown-linux-musl b",
      "target aarch64-unknown-linux-gnu b",
      "target aarch64-unknown-linux-musl b",
      "target x86_64-pc-windows-msvc",
      "target aarch64-pc-windows-msvc",
      "",
    ].join("\n");
    if (got !== want) throw new Error(`manifest:\n${got}`);
  };
  return [
    [
      "the manifest names the tree, the URL and every hosted target",
      true,
      manifestIs,
    ],
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
      packs(build("unpatched", libs, unixFiles), linux, () => "unpatched", []),
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
        [],
      ),
    ],
    [
      "a Windows build missing icudtl.dat",
      false,
      packs(
        build("noicu", libs, [...libs.map((l) => `${l}.lib`), "bindings.rs"]),
        windows,
        () => null,
        [],
      ),
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
 * This tree's `skia-binaries.txt`: the tree `PATCHES` names, and for each
 * platform the cfgs of build.rs's `SKIA_PATCHES` whose file and marker that
 * platform's check (`PATCHED` in scripts/check-patched-skia.mjs) holds a
 * build to.
 */
async function currentManifest() {
  const { treeName } = await import("./skia-source.mjs");
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
  return manifestText(treeName(), repository, cfgsFor);
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
        console.log(`skia-pack: ${MANIFEST} matches PATCHES and build.rs`);
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
      const { commit, version } = packagedSkiaBindings();
      const { PATCHED, buildLog, refusal } =
        await import("./check-patched-skia.mjs");
      const patched = PATCHED[platformOf(target)];
      const archive = pack(
        buildDir,
        outDir,
        {
          commit,
          target,
          features: features.split(","),
          crtStatic: crtStatic === "static",
          version,
        },
        () => refusal(buildLog(buildDir), patched),
      );
      console.log(`skia-pack: packed ${archive}`);
    } else {
      console.error(
        "usage: node scripts/skia-pack.mjs pack <build-dir> <out-dir> <target> <features> [static] | manifest [--check] | --self-test",
      );
      process.exit(2);
    }
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
