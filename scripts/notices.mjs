#!/usr/bin/env node
// Writes the third-party notices a shipped binary owes, from what that binary
// links, with each project's licence reproduced from the source it was built
// from rather than retyped or linked to.
//
//   node scripts/notices.mjs archive <skia out dir> <file>
//       a skia-binaries archive's: Skia and every project its build compiled,
//       read from the build's own ninja dependency record, and rust-skia's
//       C++ glue, which every archive carries
//   node scripts/notices.mjs record <skia out dir> <file>
//       the projects a Skia build compiled, as JSON, for a skia-cache entry
//       to carry where the build's ninja state does not go
//   node scripts/notices.mjs binary <triple> <file> <cargo build args>...
//       an npm binary's, for the build those arguments make: the Skia it
//       linked, every crate that target links, and the libraries it links
//       statically from the build image
//   node scripts/notices.mjs release <file>
//       `just build-release`'s binary's, on this host
//   node scripts/notices.mjs union <file> <notices>...
//       one file holding every section of the given ones, once
//   node scripts/notices.mjs package <file> <dir>
//       the main npm package's: the union of `<target>.gz.notices.md` in
//       <dir> for every target in lib/targets.json, refusing a missing one
//   node scripts/notices.mjs gn <skia tree> <gn out dir>
//       the projects `gn desc` puts under the libraries skia-bindings builds,
//       for an archive built before archives carried their notices
//   node scripts/notices.mjs check
//       refuse a shipped crate with no licence text, a `licences/` entry
//       nothing needs, and a section whose text is not its source's bytes
//   node scripts/notices.mjs --self-test
//
// What a binary links is measured, never listed by hand: an archive's
// projects are the paths its build's objects were compiled from, so a header-
// only library -- the Vulkan Memory Allocator, compiled into libskia -- is
// there because its header was read, and a project the build never touched is
// not. The table below only names where each project's licence lives.
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// Each project an archive can compile, its heading, and its licence files
/// relative to the Skia tree the build ran in. A Linux build's `freetype` and
/// `zlib` are links to `vendor/`, so their texts are read through them from
/// the commits those submodules pin.
export const SKIA_PROJECTS = {
  skia: { title: "Skia", files: ["LICENSE"] },
  skcms: { title: "skcms", files: ["modules/skcms/LICENSE"] },
  expat: { title: "Expat", files: ["third_party/externals/expat/COPYING"] },
  harfbuzz: {
    title: "HarfBuzz",
    files: ["third_party/externals/harfbuzz/COPYING"],
  },
  icu: {
    title: "ICU, its code and the data file icudtl.dat",
    files: ["third_party/externals/icu/LICENSE"],
  },
  "libjpeg-turbo": {
    title: "libjpeg-turbo",
    files: [
      "third_party/externals/libjpeg-turbo/LICENSE.md",
      "third_party/externals/libjpeg-turbo/README.ijg",
    ],
  },
  libpng: { title: "libpng", files: ["third_party/externals/libpng/LICENSE"] },
  libwebp: {
    title: "libwebp",
    files: [
      "third_party/externals/libwebp/COPYING",
      "third_party/externals/libwebp/PATENTS",
    ],
  },
  wuffs: {
    title: "Wuffs",
    files: [
      "third_party/externals/wuffs/LICENSE",
      "third_party/externals/wuffs/LICENSE-APACHE",
      "third_party/externals/wuffs/LICENSE-MIT",
    ],
  },
  zlib: { title: "zlib", files: ["third_party/externals/zlib/LICENSE"] },
  brotli: { title: "Brotli", files: ["third_party/externals/brotli/LICENSE"] },
  freetype: {
    title: "FreeType",
    files: [
      "third_party/externals/freetype/LICENSE.TXT",
      "third_party/externals/freetype/docs/FTL.TXT",
    ],
  },
  vulkanmemoryallocator: {
    title: "Vulkan Memory Allocator",
    files: ["third_party/externals/vulkanmemoryallocator/LICENSE.txt"],
  },
  "vulkan-headers": {
    title: "Vulkan headers (Khronos Group)",
    files: ["include/third_party/vulkan/LICENSE"],
  },
};

/// The project a path under `third_party/externals/<dir>/` belongs to. A
/// directory missing here refuses rather than passes unnoticed.
const EXTERNALS = {
  expat: "expat",
  harfbuzz: "harfbuzz",
  icu: "icu",
  "libjpeg-turbo": "libjpeg-turbo",
  libpng: "libpng",
  libwebp: "libwebp",
  wuffs: "wuffs",
  zlib: "zlib",
  brotli: "brotli",
  freetype: "freetype",
  vulkanmemoryallocator: "vulkanmemoryallocator",
};

/// The project each third-party gn directory builds, for `gn desc`.
const GN_DIRS = {
  "//third_party/expat": "expat",
  "//third_party/harfbuzz": "harfbuzz",
  "//third_party/icu": "icu",
  "//third_party/libjpeg-turbo": "libjpeg-turbo",
  "//third_party/libpng": "libpng",
  "//third_party/libwebp": "libwebp",
  "//third_party/wuffs": "wuffs",
  "//third_party/zlib": "zlib",
  "//third_party/brotli": "brotli",
  "//third_party/freetype2": "freetype",
  "//modules/skcms": "skcms",
  "//src/gpu/vk/vulkanmemoryallocator": "vulkanmemoryallocator",
};

/// gn labels that compile nothing into an archive: `//third_party:fontconfig`
/// only points the build at the system's fontconfig headers.
const GN_NO_CODE = new Set(["//third_party:fontconfig"]);

/// The libraries skia-bindings 0.156 builds and an archive packs
/// (binaries_config.rs), as gn targets.
const GN_TARGETS = [
  "//:skia",
  "//modules/skparagraph:skparagraph",
  "//modules/skshaper:skshaper",
  "//modules/skunicode:skunicode_core",
  "//modules/skunicode:skunicode_icu",
  "//modules/svg:svg",
  "//modules/skresources:skresources",
];

/**
 * The projects the paths in `deps` -- `ninja -t deps` output, or any text
 * naming files of a Skia tree -- belong to. Skia is always one.
 */
export function projectsFromDeps(deps) {
  const found = new Set(["skia"]);
  for (const [, dir] of deps.matchAll(/third_party\/externals\/([^/\s]+)\//g)) {
    const project = EXTERNALS[dir];
    if (!project)
      throw new Error(
        `notices: the build compiled third_party/externals/${dir}, which scripts/notices.mjs names no licence for`,
      );
    found.add(project);
  }
  if (/(^|[\s/])modules\/skcms\//m.test(deps)) found.add("skcms");
  if (/(^|[\s/])include\/third_party\/vulkan\//m.test(deps))
    found.add("vulkan-headers");
  return sortProjects(found);
}

/** The projects `gn desc <out> <target> deps --all` lines name. */
export function projectsFromGn(lines, vulkan) {
  const found = new Set(["skia"]);
  for (const line of lines.split("\n")) {
    const label = line.trim().replace(/\(.*\)$/, "");
    if (!label.startsWith("//")) continue;
    if (GN_NO_CODE.has(label)) continue;
    const dir = label.split(":")[0];
    if (GN_DIRS[dir]) found.add(GN_DIRS[dir]);
    else if (dir.startsWith("//third_party"))
      throw new Error(
        `notices: gn names ${label}, which scripts/notices.mjs names no project for`,
      );
  }
  // The headers are not a gn target; every Vulkan source reads them.
  if (vulkan) found.add("vulkan-headers");
  return sortProjects(found);
}

const sortProjects = (set) =>
  Object.keys(SKIA_PROJECTS).filter((id) => set.has(id));

/**
 * The gn arguments skia-bindings printed on its `Skia args:` line, as
 * `[name, value]`: a value is a quoted string, a bracketed list or a bare
 * word, so a path holding a space stays whole.
 */
export function parseSkiaArgs(log) {
  const line = /^Skia args: (.*)$/m.exec(log);
  if (!line) return null;
  const args = [];
  const text = line[1];
  let i = 0;
  while (i < text.length) {
    while (text[i] === " ") i++;
    if (i >= text.length) break;
    const eq = text.indexOf("=", i);
    if (eq < 0) return null;
    const name = text.slice(i, eq);
    let j = eq + 1;
    if (text[j] === '"') {
      j = text.indexOf('"', j + 1) + 1;
      if (j === 0) return null;
    } else if (text[j] === "[") {
      let inString = false;
      for (j++; j < text.length; j++) {
        if (text[j] === '"') inString = !inString;
        else if (text[j] === "]" && !inString) break;
      }
      if (j >= text.length) return null;
      j++;
    } else while (j < text.length && text[j] !== " ") j++;
    args.push([name, text.slice(eq + 1, j)]);
    i = j;
  }
  return args;
}

/**
 * The record a skia-cache entry stored before entries carried one: what
 * `gn desc` puts under the libraries skia-bindings builds, for the gn
 * arguments its seed build printed, in the Skia tree `tree`. `null` where the
 * log names no arguments; gn's own failure is thrown.
 */
export function recordFromLog(tree, log) {
  const args = parseSkiaArgs(log);
  if (!args) return null;
  const out = mkdtempSync(join(tmpdir(), "notices-gn-"));
  try {
    writeFileSync(
      join(out, "args.gn"),
      args.map(([k, v]) => `${k} = ${v}`).join("\n") + "\n",
    );
    const gn = join(
      tree,
      "bin",
      process.platform === "win32" ? "gn.exe" : "gn",
    );
    execFileSync(gn, ["gen", out], { cwd: tree, stdio: "ignore" });
    const labels = GN_TARGETS.map((t) =>
      execFileSync(gn, ["desc", out, t, "deps", "--all"], {
        cwd: tree,
        encoding: "utf8",
      }),
    ).join("\n");
    const vulkan = args.some(
      ([k, v]) => k === "skia_use_vulkan" && v === "true",
    );
    return { tree, projects: projectsFromGn(labels, vulkan) };
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}

/**
 * The Skia tree a gn build directory was generated from. `--root` is relative
 * to the build directory as the file system resolves it, so it is joined to
 * the real path: joined to a symlink's, its `..` climbs the wrong directory.
 */
export function skiaTreeOf(outDir) {
  const ninja = readFileSync(join(outDir, "build.ninja"), "utf8");
  const root = /--root=(\S+)/.exec(ninja);
  if (!root) throw new Error(`notices: ${outDir}/build.ninja names no --root`);
  return resolve(realpathSync(outDir), root[1]);
}

/** The projects a Skia build in `outDir` compiled, from ninja's own record. */
export function compiledProjects(outDir) {
  const deps = execFileSync("ninja", ["-C", outDir, "-t", "deps"], {
    encoding: "utf8",
    maxBuffer: 1 << 30,
  });
  if (!deps.trim())
    throw new Error(
      `notices: ninja holds no dependency record in ${outDir}; a build that ran there leaves one`,
    );
  return projectsFromDeps(deps);
}

/**
 * Sections for `projects`, each text read from the Skia tree `tree`. A
 * project the tree links in from `root`'s `vendor/` -- Linux builds put the
 * vendor/freetype and vendor/zlib submodules in place of Skia's copies -- is
 * a section of its own, `vendor:<name>`, naming the commit compiled: its
 * text is not the text of Skia's copy, and a file joining a Linux binary's
 * notices to a macOS one's carries both. A project resolving anywhere else
 * outside the tree is refused, having no source to name.
 */
export function skiaSections(projects, tree, root = ROOT) {
  const inside = (dir, path) => path.startsWith(dir + sep);
  const realTree = realpathSync(tree);
  const vendor = existsSync(join(root, "vendor"))
    ? realpathSync(join(root, "vendor"))
    : null;
  return projects.map((id) => {
    const { title, files } = SKIA_PROJECTS[id];
    const read = files.map((file) => ({
      name: file,
      text: readFileSync(join(tree, file), "utf8"),
    }));
    const real = realpathSync(join(tree, files[0]));
    if (inside(realTree, real)) return { id: `skia:${id}`, title, files: read };
    if (!vendor || !inside(vendor, real))
      throw new Error(
        `notices: ${files[0]} in ${tree} resolves to ${real}, outside the Skia tree and this repository's vendor/`,
      );
    const name = relative(vendor, real).split(sep)[0];
    const commit = execFileSync(
      "git",
      ["-C", join(vendor, name), "rev-parse", "HEAD"],
      {
        encoding: "utf8",
      },
    ).trim();
    return {
      id: `vendor:${name}`,
      title: `${title} (vendor/${name})`,
      note: `This build compiled ${title} from the \`vendor/${name}\` submodule of https://github.com/l7aromeo/meo-skia-canvas, at ${commit}, in place of the copy in Skia's tree.`,
      files: read,
    };
  });
}

/// Licence files a crate's package carries beside its own, where it vendors
/// another project: libaom-sys builds libaom from `vendor/`.
const VENDORED = { "libaom-sys": ["vendor/LICENSE", "vendor/PATENTS"] };

const LICENCE_FILE = /^(licen[cs]e|copying|notice|unlicense|patents)/i;

/**
 * Every crate version `triple`'s binary links, `[name, version]`, as cargo
 * resolves the release addon's features for that target: normal
 * dependencies only, the way `just licenses` counts them.
 */
export function shippedCrates(triple, features, root = ROOT) {
  const out = execFileSync(
    "cargo",
    [
      "tree",
      "--locked",
      "--prefix",
      "none",
      "-e",
      "normal",
      "--no-default-features",
      "--features",
      features,
      "--target",
      triple,
    ],
    { cwd: root, encoding: "utf8", maxBuffer: 1 << 28 },
  );
  const crates = new Map();
  for (const line of out.split("\n")) {
    const [name, version] = line.trim().split(/\s+/);
    if (version?.startsWith("v"))
      crates.set(`${name}@${version.slice(1)}`, [name, version.slice(1)]);
  }
  return [...crates.values()];
}

/** cargo metadata's packages, by `name@version`. */
export function cargoPackages(root = ROOT) {
  const meta = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--locked", "--format-version", "1", "--all-features"],
      { cwd: root, encoding: "utf8", maxBuffer: 1 << 30 },
    ),
  );
  return new Map(meta.packages.map((p) => [`${p.name}@${p.version}`, p]));
}

/**
 * Each crate's licence texts: the files its package carries, else the ones
 * `licences/crates/<name>-<version>/` holds from its repository. A crate
 * with neither is refused by name, with every other such crate.
 */
export function crateSections(crates, packages, root = ROOT) {
  const missing = [];
  const sections = [];
  for (const [name, version] of crates) {
    const pkg = packages.get(`${name}@${version}`);
    if (!pkg)
      throw new Error(`notices: cargo metadata has no ${name} ${version}`);
    if (pkg.source === null) continue; // this crate: the project's own LICENSE
    const dir = dirname(pkg.manifest_path);
    let files = readdirSync(dir)
      .filter((f) => LICENCE_FILE.test(f) && statSync(join(dir, f)).isFile())
      .sort()
      .map((f) => ({ name: f, text: readFileSync(join(dir, f), "utf8") }));
    let note;
    if (!files.length) {
      const kept = join(root, "licences", "crates", `${name}-${version}`);
      if (existsSync(kept)) {
        files = readdirSync(kept)
          .filter((f) => f !== "source.json")
          .sort()
          .map((f) => ({ name: f, text: readFileSync(join(kept, f), "utf8") }));
        // Where the text is not the crate's own file at its own commit, the
        // entry says so, and the notice carries that.
        note = JSON.parse(readFileSync(join(kept, "source.json"), "utf8")).note;
      }
    }
    for (const extra of VENDORED[name] ?? [])
      files.push({ name: extra, text: readFileSync(join(dir, extra), "utf8") });
    if (!files.length) {
      missing.push(`${name} ${version} (${pkg.license})`);
      continue;
    }
    sections.push({
      id: `crate:${name}@${version}`,
      title: `${name} ${version}`,
      ...(note && { note }),
      files,
    });
  }
  if (missing.length)
    throw new Error(
      `notices: no licence text for ${missing.length} crate(s), neither in its package nor under licences/crates: ${missing.join(", ")}`,
    );
  return sections;
}

/// The script that builds the static libraries a Linux binary links from the
/// build image, and the release of fontconfig it pins.
const FONT_RECIPE = "containers/install-fontlibs.sh";
const FONTCONFIG_PIN = /^FONTCONFIG_VERSION=(\S+)$/m;

/**
 * What a Linux binary links statically from the build image, with the text
 * kept under `licences/` for the release the recipe builds. A recipe moved to
 * a release with no text there is refused: shipping the previous release's
 * would name a fontconfig the binary does not hold. The image does not keep
 * the tarball's own COPYING to compare against; `source.json` beside the text
 * names the tarball and the sha256 the recipe verifies it by.
 */
function imageLibraries(root) {
  const pinned = FONTCONFIG_PIN.exec(
    readFileSync(join(root, FONT_RECIPE), "utf8"),
  );
  if (!pinned)
    throw new Error(`notices: ${FONT_RECIPE} pins no FONTCONFIG_VERSION`);
  const dir = `licences/fontconfig-${pinned[1]}`;
  if (!existsSync(join(root, dir)))
    throw new Error(
      `notices: ${FONT_RECIPE} builds fontconfig ${pinned[1]}, and ${dir} holds no licence text for it; add that release's COPYING with a source.json naming its tarball`,
    );
  return { fontconfig: { title: `Fontconfig ${pinned[1]}`, dir } };
}

/**
 * The musl image installs Alpine's `expat-static`, whose version the image
 * does not pin, and fontconfig's build links expat there; which copy a musl
 * binary's link takes, that one or the one compiled from Skia's tree, is not
 * recorded anywhere. Its own id, so that joining a glibc file to a musl one
 * finds no section that differs between them.
 */
const MUSL_EXPAT = {
  id: "image:expat-musl",
  title: "Expat in the musl builds",
  note: "The musl build image installs Alpine's `expat-static` package, at a version the image does not pin, and fontconfig links expat there. This binary may carry that build of Expat in place of, or beside, the one compiled from Skia's tree. Both are the Expat project's code under its MIT licence; the text under Expat above is the copy in Skia's tree.",
  files: [],
};

/** The sections for the build image's static libraries a `triple` links. */
export function imageSections(triple, root = ROOT) {
  if (!triple.includes("-linux-")) return [];
  const libraries = Object.entries(imageLibraries(root)).map(
    ([id, { title, dir }]) => ({
      id: `image:${id}`,
      title,
      files: readdirSync(join(root, dir))
        .filter((f) => f !== "source.json")
        .sort()
        .map((f) => ({
          name: f,
          text: readFileSync(join(root, dir, f), "utf8"),
        })),
    }),
  );
  return triple.endsWith("-musl") ? [...libraries, MUSL_EXPAT] : libraries;
}

/** A fence no line of `text` can close. */
function fence(text) {
  const longest = Math.max(
    0,
    ...[...text.matchAll(/`+/g)].map((m) => m[0].length),
  );
  return "`".repeat(Math.max(3, longest + 1));
}

/**
 * The notices file: an opening, then each section once, its texts verbatim
 * in fenced blocks. A section opens with a comment naming its id, which is
 * what `union` and `check` read back.
 */
export function render(opening, sections) {
  const parts = [`# Third-party notices\n\n${opening.trim()}\n`];
  for (const { id, title, note, files } of sections) {
    parts.push(`<!-- notice: ${id} -->\n## ${title}\n`);
    if (note) parts.push(`${note}\n`);
    for (const { name, text } of files) {
      const f = fence(text);
      parts.push(
        `\`${name}\`:\n\n${f}text\n${text.replace(/\n?$/, "\n")}${f}\n`,
      );
    }
  }
  return parts.join("\n");
}

/** The sections of a rendered file, by id, with their raw text. */
export function sectionsOf(rendered) {
  const sections = new Map();
  const marks = [...rendered.matchAll(/^<!-- notice: (\S+) -->$/gm)];
  marks.forEach((m, i) => {
    const end = i + 1 < marks.length ? marks[i + 1].index : rendered.length;
    sections.set(m[1], rendered.slice(m.index, end).trimEnd() + "\n");
  });
  return sections;
}

/** Every section of `files`, once, refusing one id with two texts. */
export function union(opening, files) {
  const all = new Map();
  for (const file of files)
    for (const [id, body] of sectionsOf(file)) {
      if (all.has(id) && all.get(id) !== body)
        throw new Error(
          `notices: ${id} differs between the files being joined`,
        );
      all.set(id, body);
    }
  const order = [...all.keys()].sort(
    (a, b) => rank(a) - rank(b) || a.localeCompare(b),
  );
  return `# Third-party notices\n\n${opening.trim()}\n\n${order.map((id) => all.get(id)).join("\n")}`;
}

const rank = (id) =>
  id.startsWith("skia:") || id.startsWith("vendor:")
    ? 0
    : id.startsWith("image:")
      ? 1
      : 2;

/// The opening of every notices file, naming what it covers.
const OPENING = {
  archive: (key) =>
    `The libraries in this archive, skia-binaries-${key}, are Skia and the projects its build compiled into them, with rust-skia's C++ glue. Each project's licence follows as its source tree holds it. Generated by scripts/notices.mjs in https://github.com/l7aromeo/meo-skia-canvas from the build's own dependency record.`,
  binary: (triple) =>
    `This skia.node for ${triple} links Skia and the projects its build compiled, the Rust crates the addon depends on, and on Linux the libraries it links statically from the build image. Each licence follows as its source holds it. Generated by scripts/notices.mjs in https://github.com/l7aromeo/meo-skia-canvas.`,
  union: () =>
    `The skia.node binaries this package installs or downloads, one per platform, link the projects below; each platform package carries its own platform's subset. Each licence follows as its source holds it. Generated by scripts/notices.mjs in https://github.com/l7aromeo/meo-skia-canvas.`,
};

export { OPENING };

/** rust-skia's notice, for the C++ glue every archive's skia-bindings library holds. */
function rustSkiaSections(packages) {
  const bindings = [...packages.values()].find(
    (p) => p.name === "skia-bindings",
  );
  return crateSections([[bindings.name, bindings.version]], packages);
}

/**
 * The notices file of an archive packed from the Skia build in `outDir`
 * under `key`: what that build compiled, and rust-skia's glue.
 */
export function archiveNotices(outDir, key) {
  const { tree, projects } = projectsOf(outDir);
  return render(OPENING.archive(key), [
    ...skiaSections(projects, tree),
    ...rustSkiaSections(cargoPackages()),
  ]);
}

/** The archive key `out/skia/../..`'s build packed, if the caller names none. */
const archiveKey = (dir) =>
  existsSync(join(dir, "key.txt"))
    ? readFileSync(join(dir, "key.txt"), "utf8").trim()
    : null;

/** The triple's release features, from `lib/prebuild.mjs`. */
async function featuresFor(triple) {
  const { CARGO_FEATURES_BY_OS } = await import("../lib/prebuild.mjs");
  const os = triple.includes("apple")
    ? "darwin"
    : triple.includes("windows")
      ? "win32"
      : "linux";
  return CARGO_FEATURES_BY_OS[os];
}

/** Every triple a shipped target builds for, from `lib/targets.json`. */
export function shippedTriples(root = ROOT) {
  const targets = JSON.parse(
    readFileSync(join(root, "lib", "targets.json"), "utf8"),
  );
  const arch = { x64: "x86_64", arm64: "aarch64" };
  return [
    ...new Set(
      Object.values(targets).map(({ os, cpu, libc }) => {
        const a = arch[cpu[0]];
        if (os[0] === "darwin") return `${a}-apple-darwin`;
        if (os[0] === "win32") return `${a}-pc-windows-msvc`;
        return `${a}-unknown-linux-${libc?.[0] === "musl" ? "musl" : "gnu"}`;
      }),
    ),
  ];
}

/**
 * What the Skia build in `outDir` compiled and the tree it compiled from:
 * the record a skia-cache entry keeps, since a build that links the entry
 * runs no ninja of its own.
 */
export function record(outDir) {
  return { tree: skiaTreeOf(outDir), projects: compiledProjects(outDir) };
}

/**
 * Where the Skia an addon build linked is described: the cache entry it
 * linked, which keeps a record, or the build's own `out/skia`, which keeps
 * ninja's. Asked of cargo, by running the build again with `cargoArgs` --
 * the same arguments, so nothing rebuilds -- and reading skia-bindings'
 * `build-script-executed` message, never by guessing a build path.
 */
export function skiaOfBuild(cargoArgs) {
  const out = execFileSync(
    process.execPath,
    [
      join(ROOT, "scripts", "skia-cache.mjs"),
      "cargo",
      "build",
      "--message-format=json",
      ...cargoArgs,
    ],
    {
      cwd: ROOT,
      encoding: "utf8",
      maxBuffer: 1 << 30,
      stdio: ["ignore", "pipe", "inherit"],
    },
  );
  for (const line of out.split("\n")) {
    if (!line.startsWith("{")) continue;
    const message = JSON.parse(line);
    if (message.reason !== "build-script-executed") continue;
    if (!/[/#]skia-bindings[@#]/.test(message.package_id)) continue;
    const linked = message.linked_paths
      .map((path) => path.replace(/^[a-z]+=/, ""))
      .filter((dir) => existsSync(join(dir, "notices.json")));
    if (linked.length) return linked[0];
    const skia = join(message.out_dir, "skia");
    if (existsSync(join(skia, "build.ninja"))) return skia;
    throw new Error(
      `notices: skia-bindings linked neither a cache entry with notices.json nor a Skia it built in ${skia}`,
    );
  }
  throw new Error(
    "notices: cargo ran no skia-bindings build script for these arguments",
  );
}

/** The Skia projects of a build directory, or of a skia-cache entry's record. */
function projectsOf(dir) {
  if (existsSync(join(dir, "notices.json")))
    return JSON.parse(readFileSync(join(dir, "notices.json"), "utf8"));
  if (existsSync(join(dir, "build.ninja"))) return record(dir);
  throw new Error(
    `notices: ${dir} is neither a Skia build directory nor a skia-cache entry with notices.json`,
  );
}

/**
 * The check `just ci` runs: every crate each shipped target links has a
 * licence text, and every directory under `licences/crates` is one of those
 * crates and says where its texts came from.
 */
async function check() {
  const packages = cargoPackages();
  const needed = new Set();
  const failures = [];
  for (const triple of shippedTriples()) {
    const crates = shippedCrates(triple, await featuresFor(triple));
    try {
      crateSections(crates, packages);
    } catch (error) {
      failures.push(`${triple}: ${error.message}`);
    }
    for (const [name, version] of crates) needed.add(`${name}-${version}`);
  }
  try {
    imageSections(shippedTriples().find((t) => t.includes("-linux-")));
  } catch (error) {
    failures.push(error.message);
  }
  const kept = join(ROOT, "licences", "crates");
  for (const dir of existsSync(kept) ? readdirSync(kept) : []) {
    if (!needed.has(dir))
      failures.push(
        `licences/crates/${dir} is a crate no shipped target links`,
      );
    const source = join(kept, dir, "source.json");
    if (!existsSync(source))
      failures.push(
        `licences/crates/${dir} says nowhere where its texts came from`,
      );
    else {
      const { repository, commit, files } = JSON.parse(
        readFileSync(source, "utf8"),
      );
      if (!repository || !commit || !files || !Object.keys(files).length)
        failures.push(
          `licences/crates/${dir}/source.json names no repository, commit and files`,
        );
      for (const name of Object.keys(files ?? {}))
        if (!existsSync(join(kept, dir, name)))
          failures.push(`licences/crates/${dir} lacks ${name}`);
    }
  }
  if (failures.length) {
    console.error(failures.map((f) => `::error::${f}`).join("\n"));
    process.exit(1);
  }
  console.log(
    `notices: every crate the ${shippedTriples().length} shipped targets link has its licence text, the fontconfig the image builds has its own, and licences/ holds nothing else`,
  );
}

function selfTest() {
  const refuse = (what, f) => {
    try {
      f();
    } catch {
      return;
    }
    throw new Error(`self-test: ${what} was not refused`);
  };
  const eq = (what, got, want) => {
    if (JSON.stringify(got) !== JSON.stringify(want))
      throw new Error(
        `self-test: ${what}: ${JSON.stringify(got)}, wanted ${JSON.stringify(want)}`,
      );
  };
  eq(
    "ninja deps",
    projectsFromDeps(
      "obj/a.o: #deps 3\n    ../../third_party/externals/harfbuzz/src/hb.cc\n    ../../modules/skcms/skcms.cc\n    ../../include/third_party/vulkan/vulkan/vulkan_core.h\n    ../../third_party/externals/freetype/src/base/ftbase.c\n",
    ),
    ["skia", "skcms", "harfbuzz", "freetype", "vulkan-headers"],
  );
  eq(
    "nothing but Skia",
    projectsFromDeps("obj/a.o: #deps 1\n    ../../src/core/SkCanvas.cpp\n"),
    ["skia"],
  );
  refuse("an external with no licence named", () =>
    projectsFromDeps("    ../../third_party/externals/dawn/src/x.cpp\n"),
  );
  eq(
    "gn labels",
    projectsFromGn(
      "//third_party/zlib:zlib\n//third_party:fontconfig\n//src/gpu/vk/vulkanmemoryallocator:vulkanmemoryallocator\n//modules/skcms:skcms(//gn/toolchain:x)\n",
      true,
    ),
    ["skia", "skcms", "zlib", "vulkanmemoryallocator", "vulkan-headers"],
  );
  refuse("a gn third_party label with no project", () =>
    projectsFromGn("//third_party/dawn:dawn\n", false),
  );
  const text = "a ``` b\n````\n";
  const out = render("Opening.", [
    { id: "x:y", title: "Y", files: [{ name: "LICENSE", text }] },
  ]);
  if (!out.includes("`````text\n" + text + "`````"))
    throw new Error(
      "self-test: a text holding four backticks is not fenced by five",
    );
  eq("sections read back", [...sectionsOf(out).keys()], ["x:y"]);
  const other = render("O.", [
    { id: "a:b", title: "B", files: [{ name: "L", text: "b\n" }] },
    { id: "x:y", title: "Y", files: [{ name: "LICENSE", text }] },
  ]);
  eq(
    "union keeps each once",
    [...sectionsOf(union("U.", [out, other])).keys()],
    ["a:b", "x:y"],
  );
  refuse("one id with two texts", () =>
    union("U.", [
      out,
      render("O.", [
        {
          id: "x:y",
          title: "Y",
          files: [{ name: "LICENSE", text: "changed\n" }],
        },
      ]),
    ]),
  );
  eq(
    "Skia args, a path with a space and a list",
    parseSkiaArgs(
      'x\nSkia args: a=true cc="clang" extra_cflags=["--sysroot=/A B/sdk","-O3"] target_os="mac"\ny',
    ),
    [
      ["a", "true"],
      ["cc", '"clang"'],
      ["extra_cflags", '["--sysroot=/A B/sdk","-O3"]'],
      ["target_os", '"mac"'],
    ],
  );
  eq("no Skia args line", parseSkiaArgs("cargo:rerun-if-changed=x"), null);
  const noted = render("O.", [
    {
      id: "c:d",
      title: "D",
      note: "Not its own file.",
      files: [{ name: "LICENSE", text: "t\n" }],
    },
  ]);
  if (!noted.includes("## D\n\nNot its own file.\n"))
    throw new Error(
      "self-test: a section's note is not printed under its heading",
    );
  // gn writes `--root` relative to the build directory as the file system
  // resolves it, so `..` from a symlinked build directory climbs the real one.
  const real = realpathSync(mkdtempSync(join(tmpdir(), "notices-tree-")));
  const elsewhere = mkdtempSync(join(tmpdir(), "notices-link-"));
  try {
    mkdirSync(join(real, "out", "skia"), { recursive: true });
    writeFileSync(
      join(real, "out", "skia", "build.ninja"),
      "rule gn\n  command = gn --root=../.. -q gen .\n",
    );
    symlinkSync(join(real, "out", "skia"), join(elsewhere, "skia"));
    eq(
      "the tree of a symlinked build directory",
      skiaTreeOf(join(elsewhere, "skia")),
      real,
    );
  } finally {
    rmSync(real, { recursive: true, force: true });
    rmSync(elsewhere, { recursive: true, force: true });
  }
  eq(
    "the image's sections, glibc and musl",
    [
      imageSections("x86_64-unknown-linux-gnu"),
      imageSections("x86_64-unknown-linux-musl"),
    ].map((s) => s.map((x) => x.id)),
    [["image:fontconfig"], ["image:fontconfig", "image:expat-musl"]],
  );
  const glibcFile = render("G.", imageSections("x86_64-unknown-linux-gnu"));
  const muslFile = render("M.", imageSections("x86_64-unknown-linux-musl"));
  eq(
    "glibc and musl join",
    [...sectionsOf(union("U.", [glibcFile, muslFile])).keys()],
    ["image:expat-musl", "image:fontconfig"],
  );
  // A project the tree links in from this repository's vendor/ is its own
  // section, so a Linux file joins a macOS one; one from anywhere else is
  // refused, having no name to give it.
  const scratch = realpathSync(mkdtempSync(join(tmpdir(), "notices-vendor-")));
  try {
    const tree = join(scratch, "skia");
    const vendored = join(scratch, "root", "vendor", "zlib");
    mkdirSync(join(tree, "third_party", "externals"), { recursive: true });
    mkdirSync(vendored, { recursive: true });
    writeFileSync(join(vendored, "LICENSE"), "vendored\n");
    const git = (...a) =>
      execFileSync(
        "git",
        ["-C", vendored, "-c", "user.name=n", "-c", "user.email=n@n", ...a],
        { encoding: "utf8" },
      ).trim();
    git("init", "-q");
    git("add", "LICENSE");
    git("commit", "-q", "-m", "x");
    symlinkSync(vendored, join(tree, "third_party", "externals", "zlib"));
    const [section] = skiaSections(["zlib"], tree, join(scratch, "root"));
    eq("a vendored project's id", section.id, "vendor:zlib");
    if (!section.note?.includes(git("rev-parse", "HEAD")))
      throw new Error(
        "self-test: a vendored project's note does not name the commit compiled",
      );
    eq(
      "its text",
      section.files.map((f) => f.text),
      ["vendored\n"],
    );
    refuse("a project linked in from outside the tree and vendor/", () =>
      skiaSections(["zlib"], tree, join(scratch, "elsewhere")),
    );
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  // The fontconfig text is the one for the release the image's recipe
  // builds: a recipe moved to a release licences/ holds no text for is
  // refused, rather than shipping the old release's.
  const recipeRoot = mkdtempSync(join(tmpdir(), "notices-recipe-"));
  try {
    mkdirSync(join(recipeRoot, "containers"));
    mkdirSync(join(recipeRoot, "licences", "fontconfig-2.17.1"), {
      recursive: true,
    });
    writeFileSync(
      join(recipeRoot, "licences", "fontconfig-2.17.1", "COPYING"),
      "text\n",
    );
    const recipe = (version) =>
      writeFileSync(
        join(recipeRoot, "containers", "install-fontlibs.sh"),
        `FONTCONFIG_VERSION=${version}\n`,
      );
    recipe("2.17.1");
    eq(
      "the recipe's fontconfig",
      imageSections("x86_64-unknown-linux-gnu", recipeRoot)[0].title,
      "Fontconfig 2.17.1",
    );
    recipe("2.18.0");
    refuse("a fontconfig release licences/ holds no text for", () =>
      imageSections("x86_64-unknown-linux-gnu", recipeRoot),
    );
  } finally {
    rmSync(recipeRoot, { recursive: true, force: true });
  }
  console.log("notices: self-test, every case answered and every refusal held");
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (command === "--self-test") return selfTest();
  if (command === "check") return check();
  if (command === "record") {
    const [outDir, file] = args;
    writeFileSync(file, JSON.stringify(record(outDir), null, 2) + "\n");
    return;
  }
  if (command === "archive") {
    const [outDir, file, key] = args;
    writeFileSync(
      file,
      archiveNotices(outDir, key ?? archiveKey(outDir) ?? "this archive"),
    );
    return;
  }
  if (command === "release") {
    // `just build-release`'s binary, on this host: the triple rustc builds
    // for and the arguments lib/prebuild.mjs gives cargo.
    const [file] = args;
    const { CARGO_FEATURES } = await import("../lib/prebuild.mjs");
    const host = /^host: (\S+)$/m.exec(
      execFileSync("rustc", ["-vV"], { encoding: "utf8" }),
    )[1];
    const { tree, projects } = projectsOf(
      skiaOfBuild(["--release", "--features", CARGO_FEATURES]),
    );
    writeFileSync(
      file,
      render(OPENING.binary(host), [
        ...skiaSections(projects, tree),
        ...imageSections(host),
        ...crateSections(
          shippedCrates(host, await featuresFor(host)),
          cargoPackages(),
        ),
      ]),
    );
    return;
  }
  if (command === "binary") {
    // The addon build's own cargo arguments follow the file, so the Skia it
    // linked is the one described.
    const [triple, file, ...cargoArgs] = args;
    const { tree, projects } = projectsOf(skiaOfBuild(cargoArgs));
    const packages = cargoPackages();
    const sections = [
      ...skiaSections(projects, tree),
      ...imageSections(triple),
      ...crateSections(
        shippedCrates(triple, await featuresFor(triple)),
        packages,
      ),
    ];
    writeFileSync(file, render(OPENING.binary(triple), sections));
    return;
  }
  if (command === "package") {
    // The main npm package's: every target's, which must all be there.
    const [file, dir] = args;
    const targets = Object.keys(
      JSON.parse(readFileSync(join(ROOT, "lib", "targets.json"), "utf8")),
    );
    const inputs = targets.map((t) => join(dir, `${t}.gz.notices.md`));
    const missing = inputs.filter((f) => !existsSync(f));
    if (missing.length)
      throw new Error(
        `notices: no notices for ${missing.map((f) => f.split("/").pop()).join(", ")}; the package installs those binaries too`,
      );
    writeFileSync(
      file,
      union(
        OPENING.union(),
        inputs.map((f) => readFileSync(f, "utf8")),
      ),
    );
    return;
  }
  if (command === "union") {
    const [file, ...inputs] = args;
    writeFileSync(
      file,
      union(
        OPENING.union(),
        inputs.map((f) => readFileSync(f, "utf8")),
      ),
    );
    return;
  }
  if (command === "gn") {
    const [tree, outDir] = args;
    const args_gn = readFileSync(join(outDir, "args.gn"), "utf8");
    const labels = GN_TARGETS.map((t) =>
      execFileSync(
        join(tree, "bin", "gn"),
        ["desc", outDir, t, "deps", "--all"],
        { cwd: tree, encoding: "utf8" },
      ),
    ).join("\n");
    console.log(
      JSON.stringify(
        {
          tree,
          projects: projectsFromGn(
            labels,
            /skia_use_vulkan\s*=\s*true/.test(args_gn),
          ),
        },
        null,
        2,
      ),
    );
    return;
  }
  console.error(
    "usage: node scripts/notices.mjs archive|record|binary|union|gn|check|--self-test",
  );
  process.exit(2);
}

if (process.argv[1] === fileURLToPath(import.meta.url))
  main().catch((error) => {
    console.error(`::error::${error.message}`);
    process.exit(1);
  });
