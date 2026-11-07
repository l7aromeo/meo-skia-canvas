#!/usr/bin/env node
// Whether a build compiled Skia from the patched source, read from what
// skia-bindings' build script printed.
//
//   node scripts/check-patched-skia.mjs <target/<profile>/build>
//   node scripts/check-patched-skia.mjs --self-test
//
// rust-skia's prebuilt Skia waits one second for each Metal shader compile
// and drops the draw when it runs out, its atlas path renderer can move a
// pixel of a path's coverage a step up or down on Metal and on Vulkan, and its
// triangulating path renderer reuses a finer triangulation a larger draw of a
// curve left, and its asynchronous read allocates a fresh transfer buffer for
// every `getImageData`; a binary built against it passes every test that does
// not stall the compiler, land a path where the pixel moves, draw a curve at
// two scales or look at which buffer a read kept. Three things have to hold,
// and each is read from what skia-bindings' build script printed -- its
// `output` and its `stderr`, see `buildLog`:
//
// 1. It built from a source tree and did not try a download: offline from
//    `SKIA_SOURCE_DIR` -- "STARTING OFFLINE BUILD" -- or, from a rust-skia
//    checkout, which has no crates.io package hash to download against, from
//    the tree inside it -- "STARTING A FULL BUILD".
// 2. ninja compiled each file one of this platform's patches changes -- see
//    `PATCHED`. A build with no GPU backend compiles none under Ganesh, and
//    is held to the others: the SVG slant and the face match -- see
//    `applicable`.
// 3. Each file it compiled, resolved against the directory ninja ran in, is
//    the patched one: it holds what only the patch puts there. A build from
//    an unpatched tree passes the first two and fails this.
//
// The self-test also holds each marker to its patch: it must be in what the
// patch writes and nowhere in what it replaces or keeps, or an unpatched file
// would pass the third check.
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// containers/skia-svg-synthetic-slant.patch, on every platform.
const SVG_SYNTHETIC_SLANT = {
  patch: "containers/skia-svg-synthetic-slant.patch",
  file: "modules/svg/src/SkSVGText.cpp",
  marker: "!Leans(font.getTypeface())",
};

/// containers/skia-font-match-slant-first.patch, on every platform.
const FONT_MATCH_SLANT_FIRST = {
  patch: "containers/skia-font-match-slant-first.patch",
  file: "src/core/SkFontMgr.cpp",
  marker: "currentScore <<= kWeightScoreBits;",
};

/**
 * The entries of `SKIA_PATCHES` in `rust`, build.rs's text, by cfg: each
 * `SkiaPatch { cfg, file, marker }` with all three a plain string literal.
 * Refused, with why, where the table is missing or empty, an entry lacks a
 * field, or two entries share a cfg.
 */
export function skiaPatchTable(rust) {
  const table = rust.match(
    /^const SKIA_PATCHES: \[SkiaPatch; \d+\] = \[([\s\S]*?)^\];/m,
  );
  if (!table) return { error: "no SKIA_PATCHES table" };
  const literal = (body, field) =>
    body.match(
      new RegExp(`^\\s*${field}: "((?:[^"\\\\]|\\\\.)*)",$`, "m"),
    )?.[1];
  const entries = {};
  for (const [, body] of table[1].matchAll(/SkiaPatch \{([^}]*)\}/g)) {
    const [cfg, file, marker] = ["cfg", "file", "marker"].map((field) =>
      literal(body, field),
    );
    if (!cfg || !file || !marker)
      return { error: `an entry without a literal cfg, file and marker` };
    if (entries[cfg]) return { error: `two entries for ${cfg}` };
    entries[cfg] = { file, marker };
  }
  if (!Object.keys(entries).length) return { error: "an empty SKIA_PATCHES" };
  return { entries };
}

/**
 * The file and marker build.rs names for the patch whose cfg is `cfg`, from
 * which it turns that cfg on.
 */
function buildRsPatch(cfg) {
  const read = skiaPatchTable(readFileSync(join(ROOT, "build.rs"), "utf8"));
  if (read.error)
    throw new Error(`check-patched-skia: build.rs: ${read.error}`);
  const entry = read.entries[cfg];
  if (!entry) throw new Error(`check-patched-skia: build.rs names no ${cfg}`);
  return entry;
}

// The patches a test holds a build to. Their files and markers are build.rs's,
// which turns a cfg on from each, so the build and this check cannot disagree
// about what a patched file holds.

/// containers/skia-atlas-size.patch, on every platform.
const ATLAS_SIZE = {
  patch: "containers/skia-atlas-size.patch",
  ...buildRsPatch("meo_atlas_size"),
};

/// containers/skia-triangulation-tolerance.patch, on every platform.
const TRIANGULATION = {
  patch: "containers/skia-triangulation-tolerance.patch",
  ...buildRsPatch("meo_triangulation_tolerance"),
};

/// containers/skia-async-transfer-reuse.patch, on macOS and Linux.
const ASYNC_TRANSFER_REUSE = {
  patch: "containers/skia-async-transfer-reuse.patch",
  ...buildRsPatch("meo_async_transfer_reuse"),
};

/// Each file a patch for the platform changes, under Skia's source root, with
/// what only the patched file contains, and the patch that puts it there.
/// `just check-skia-patches` refuses a patch in `PATCHES` that is neither
/// here nor in `UNVERIFIED`.
export const PATCHED = {
  darwin: [
    {
      patch: "containers/skia-metal-untimed-compile.patch",
      file: "src/gpu/ganesh/mtl/GrMtlUtil.mm",
      marker: "GrMtlCompilesWithoutTimeout",
    },
    ATLAS_SIZE,
    TRIANGULATION,
    ASYNC_TRANSFER_REUSE,
    SVG_SYNTHETIC_SLANT,
    FONT_MATCH_SLANT_FIRST,
  ],
  linux: [
    ATLAS_SIZE,
    TRIANGULATION,
    ASYNC_TRANSFER_REUSE,
    SVG_SYNTHETIC_SLANT,
    FONT_MATCH_SLANT_FIRST,
  ],
  win32: [
    ATLAS_SIZE,
    TRIANGULATION,
    {
      patch: "containers/skia-dwrite-oblique.patch",
      file: "src/ports/SkTypeface_win_dw.cpp",
      marker: "fsSelection.field.Oblique",
    },
    SVG_SYNTHETIC_SLANT,
    FONT_MATCH_SLANT_FIRST,
  ],
};

/// The patches a build's output cannot vouch for, and why. Each still fails
/// its build loudly without it, or changes nothing a build compiles.
export const UNVERIFIED = {
  "containers/skia-brotli-sources.patch":
    "it changes a build list rather than a compiled file, and a `freetype` build without it fails to link",
  "containers/rust-skia-graphite-bindings.patch":
    "it changes rust-skia's bindings rather than Skia, and `cfg(meo_graphite)` is what says whether a build took it",
};

/**
 * The ninja line that names `file` compiled, capturing the path it gave: with
 * either separator, since one on Windows may give `\`.
 */
function compiledLine(file) {
  const escaped = file
    .split("/")
    .map((part) => part.replace(/\./g, "\\."))
    .join("[\\\\/]");
  return new RegExp(`compile (\\S+[\\\\/]${escaped})`);
}
/// The line each ninja prints for `-C`, capturing the directory. ninja writes
/// ``ninja: Entering directory `<dir>'``. samurai, which Alpine installs as
/// `/usr/bin/ninja` and so the musl container builds with, writes
/// `<the name it ran as>: entering directory '<dir>'` -- `ninja` there, `samu`
/// when run as itself -- and writes it to stderr, which is why `buildLog`
/// reads both streams. A CRLF line end needs nothing here: `$` in multiline
/// mode matches before `\r` as before `\n`, both being line terminators to
/// JavaScript.
const NINJA_DIRS = [
  /^ninja: Entering directory `(.*)'$/m,
  /^(?:ninja|samu): entering directory '(.*)'$/m,
];

/** The directory ninja ran in, from the first ninja's line that names one. */
const ninjaDir = (output) =>
  NINJA_DIRS.map((line) => output.match(line)?.[1]).find(Boolean);

const FROM_SOURCE = ["STARTING OFFLINE BUILD", "STARTING A FULL BUILD"];

/// The gn argument skia-bindings passes for a build with no GPU backend
/// (`skia_enable_ganesh`, skia-bindings' build_support/skia/config.rs), and
/// the directory of the sources only Ganesh compiles. Such a build compiles
/// none of the patched files under it, and is held to the others.
const NO_GANESH = "skia_enable_ganesh=false";
const GANESH_SOURCES = "src/gpu/ganesh/";

/**
 * The entries of `patched` the build that printed `output` compiles: all of
 * them, but for a build its gn arguments say has no Ganesh, those outside
 * `GANESH_SOURCES`.
 */
export function applicable(patched, output) {
  return output.includes(NO_GANESH)
    ? patched.filter(({ file }) => !file.startsWith(GANESH_SOURCES))
    : patched;
}

/**
 * Why `output` does not show Skia built with `patched`, or `null` when it
 * does -- each of `patched` the build compiles, see `applicable`.
 */
export function refusal(output, patched) {
  if (!FROM_SOURCE.some((line) => output.includes(line)))
    return "skia-bindings did not build from a source tree";
  if (output.includes("DOWNLOAD AND INSTALL"))
    return "skia-bindings tried to download prebuilt Skia";
  const ninja = ninjaDir(output);
  for (const { file, marker } of applicable(patched, output)) {
    const compiled = output.match(compiledLine(file))?.[1];
    if (!compiled) return `ninja did not compile ${file}`;
    if (!ninja) return "the output does not say where ninja ran";
    // `/` resolves on every platform, where `\` is a separator on Windows
    // alone.
    const path = resolve(ninja, compiled.replaceAll("\\", "/"));
    if (!existsSync(path)) return `the compiled ${path} is gone`;
    if (!readFileSync(path, "utf8").includes(marker))
      return `${path} is not the patched source`;
  }
  return null;
}

/// Where a patch file names Skia's sources: rust-skia's checkout keeps them
/// under `skia-bindings/skia`.
const SKIA_IN_RUST_SKIA = "skia-bindings/skia/";

/**
 * Why `marker` does not show that `patchText` was applied to `file`, or
 * `null` when it does: the marker has to be in the lines the patch writes
 * there and in none it replaces or keeps as context, which is all of the
 * unpatched file a patch shows.
 */
export function markerRefusal(patchText, file, marker) {
  const sections = patchText.split(/^(?=--- )/m);
  const section = sections.find((text) =>
    text.startsWith(`--- a/${SKIA_IN_RUST_SKIA}${file}\n`),
  );
  if (!section) return `the patch does not change ${file}`;
  const lines = section
    .split("\n")
    .filter((line) => !/^(---|\+\+\+|@@)/.test(line));
  const before = lines.filter((line) => !line.startsWith("+")).join("\n");
  const after = lines.filter((line) => !line.startsWith("-")).join("\n");
  if (!after.includes(marker)) return `the patch does not write ${marker}`;
  if (before.includes(marker))
    return `the unpatched ${file} already holds ${marker}`;
  return null;
}

/**
 * What skia-bindings' build script printed in `dir`, both streams: cargo
 * keeps its stdout as `output` and its stderr as `stderr`. Both ninjas print
 * what they compiled on stdout, but samurai prints the directory it ran in on
 * stderr.
 */
export function buildLog(dir) {
  const stderr = join(dir, "stderr");
  return [
    readFileSync(join(dir, "output"), "utf8"),
    existsSync(stderr) ? readFileSync(stderr, "utf8") : "",
  ].join("\n");
}

function check(buildDir) {
  const patched = PATCHED[process.platform];
  if (!patched) {
    console.error(`check-patched-skia: no patches for ${process.platform}`);
    process.exit(1);
  }
  const outputs = readdirSync(buildDir)
    .filter((name) => name.startsWith("skia-bindings-"))
    .map((name) => join(buildDir, name, "output"))
    .filter(existsSync);
  if (!outputs.length) {
    console.error(
      `check-patched-skia: no skia-bindings output under ${buildDir}`,
    );
    process.exit(1);
  }
  const problems = outputs
    .map((path) => [path, refusal(buildLog(dirname(path)), patched)])
    .filter(([, why]) => why);
  for (const [path, why] of problems) console.error(`${path}: ${why}`);
  if (problems.length) process.exit(1);
  console.log(
    `check-patched-skia: ${outputs.length} skia-bindings build(s) compiled the patched Skia source`,
  );
}

/** The self-test's cases for one platform's `patched` list, under `root`. */
function cases(root, patched) {
  const ninja = join(root, "out", "skia");
  const relative = (file) => `../../skia/${file}`;
  const files = Object.fromEntries(
    patched.map(({ file, marker }) => [file, `before ${marker} after`]),
  );
  const good = [
    "STARTING OFFLINE BUILD",
    `ninja: Entering directory \`${ninja}'`,
    ...patched.map(
      ({ file }, i) => `[${i + 1}/${patched.length}] compile ${relative(file)}`,
    ),
  ].join("\n");
  const crlf = good.replaceAll("\n", "\r\n");
  // The same build's output with samurai's directory line, as `name`.
  const samurai = (name) =>
    good.replace(
      /^ninja: Entering directory `(.*)'$/m,
      (_, dir) => `${name}: entering directory '${dir}'`,
    );
  const backslashed = good.replace(
    /compile (\S+)/g,
    (_, path) => `compile ${path.replaceAll("/", "\\")}`,
  );
  // A build with no Ganesh, which compiles only the patched files outside it.
  const ganeshless = [
    "STARTING OFFLINE BUILD",
    `Skia args: is_debug=false ${NO_GANESH} skia_enable_pdf=true`,
    `ninja: Entering directory \`${ninja}'`,
    ...patched
      .filter(({ file }) => !file.startsWith(GANESH_SOURCES))
      .map(({ file }, i) => `[${i + 1}/0] compile ${relative(file)}`),
  ].join("\n");
  const outsideGanesh = patched.filter(
    ({ file }) => !file.startsWith(GANESH_SOURCES),
  );
  return [
    [files, good, true, "an offline build of every patched file"],
    [files, ganeshless, true, "a build with no Ganesh"],
    [
      files,
      ganeshless.replace(NO_GANESH, "skia_enable_ganesh=true"),
      patched.length === outsideGanesh.length,
      "a build with Ganesh that compiled none of its patched files",
    ],
    ...outsideGanesh.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      ganeshless,
      false,
      `a build with no Ganesh of an unpatched ${file}`,
    ]),
    ...outsideGanesh.map(({ file }) => [
      files,
      ganeshless
        .split("\n")
        .filter((line) => !line.endsWith(relative(file)))
        .join("\n"),
      false,
      `a build with no Ganesh that did not compile ${file}`,
    ]),
    [files, crlf, true, "an offline build with CRLF line ends"],
    [files, backslashed, true, "an offline build naming files with `\\`"],
    ...patched.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      crlf,
      false,
      `an offline build with CRLF line ends of an unpatched ${file}`,
    ]),
    [
      files,
      good.replace("STARTING OFFLINE BUILD", "STARTING A FULL BUILD"),
      true,
      "a full build of every patched file",
    ],
    ...patched.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      good,
      false,
      `an offline build of an unpatched ${file}`,
    ]),
    ...patched.map(({ file }) => [
      files,
      good
        .split("\n")
        .filter((line) => !line.endsWith(relative(file)))
        .join("\n"),
      false,
      `no ${file} compiled`,
    ]),
    [
      files,
      good.replace(
        "STARTING OFFLINE BUILD",
        "STARTING BIND AGAINST SYSTEM SKIA",
      ),
      false,
      "a bind against a system Skia",
    ],
    [
      files,
      good.replace(
        "STARTING OFFLINE BUILD",
        "TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES",
      ),
      false,
      "a download",
    ],
    [
      files,
      `${good}\nDOWNLOAD AND INSTALL FAILED: curl`,
      false,
      "a download tried alongside",
    ],
    [
      files,
      good.replace(/^ninja: Entering.*$/m, ""),
      false,
      "no ninja directory",
    ],
    [files, samurai("ninja"), true, "samurai run as ninja"],
    [files, samurai("samu"), true, "samurai run as samu"],
    [
      files,
      samurai("ninja").replaceAll("\n", "\r\n"),
      true,
      "samurai with CRLF line ends",
    ],
    ...patched.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      samurai("ninja"),
      false,
      `samurai's build of an unpatched ${file}`,
    ]),
    [
      files,
      good.replace(
        /^ninja: Entering directory `(.*)'$/m,
        (_, dir) => `make: Entering directory '${dir}'`,
      ),
      false,
      "only make's directory line",
    ],
  ];
}

function selfTest() {
  const root = join(tmpdir(), `check-patched-skia-${process.pid}`);
  mkdirSync(join(root, "out", "skia"), { recursive: true });
  let count = 0;
  const patchText = (patch) => readFileSync(join(ROOT, patch), "utf8");
  const markerCase = (what, text, file, marker, accepted) => {
    if ((markerRefusal(text, file, marker) === null) !== accepted) {
      console.error(
        `check-patched-skia self-test: ${what} was ${accepted ? "refused" : "accepted"}`,
      );
      process.exit(1);
    }
    count += 1;
  };
  for (const [platform, patched] of Object.entries(PATCHED))
    for (const { patch, file, marker } of patched)
      markerCase(
        `${platform}'s marker for ${file}`,
        patchText(patch),
        file,
        marker,
        true,
      );
  markerCase(
    "a marker the unpatched file already holds",
    patchText(ASYNC_TRANSFER_REUSE.patch),
    ASYNC_TRANSFER_REUSE.file,
    "kDynamic_GrAccessPattern",
    false,
  );
  markerCase(
    "a marker the patch does not write",
    patchText(ATLAS_SIZE.patch),
    ATLAS_SIZE.file,
    "fAtlasMaxSize = 1024;",
    false,
  );
  // Two files, the second given the marker: the first is read from its own
  // lines, not from the patch's.
  const twoFiles = [
    `--- a/${SKIA_IN_RUST_SKIA}src/b.cpp`,
    `+++ b/${SKIA_IN_RUST_SKIA}src/b.cpp`,
    "@@ -1 +1 @@",
    "-old",
    "+new",
    `--- a/${SKIA_IN_RUST_SKIA}src/a.cpp`,
    `+++ b/${SKIA_IN_RUST_SKIA}src/a.cpp`,
    "@@ -1 +1 @@",
    "-old",
    "+MARKER",
    "",
  ].join("\n");
  markerCase(
    "a marker in the file that holds it",
    twoFiles,
    "src/a.cpp",
    "MARKER",
    true,
  );
  markerCase(
    "a marker in another file of the patch",
    twoFiles,
    "src/b.cpp",
    "MARKER",
    false,
  );
  markerCase(
    "a file the patch does not change",
    twoFiles,
    "src/c.cpp",
    "MARKER",
    false,
  );
  // build.rs's table as rustfmt writes it; anything else is refused rather
  // than read as some other entry.
  const entry = (cfg, file, marker) =>
    `    // a comment\n    SkiaPatch {\n        cfg: "${cfg}",\n        file: "${file}",\n        marker: "${marker}",\n    },\n`;
  const table = (...entries) =>
    `const SKIA_PATCHES: [SkiaPatch; ${entries.length}] = [\n${entries.join("")}];\n`;
  for (const [rust, expected, what] of [
    [
      table(entry("a", "f.cpp", "x::y;")),
      { a: { file: "f.cpp", marker: "x::y;" } },
      "one entry",
    ],
    [
      table(entry("a", "f.cpp", "x"), entry("b", "g.cpp", "p->q == r")),
      {
        a: { file: "f.cpp", marker: "x" },
        b: { file: "g.cpp", marker: "p->q == r" },
      },
      "two entries",
    ],
    ["const OTHER: [SkiaPatch; 0] = [\n];\n", null, "no table"],
    ["const SKIA_PATCHES: [SkiaPatch; 0] = [\n];\n", null, "an empty table"],
    [
      table(entry("a", "f.cpp", "x"), entry("a", "g.cpp", "y")),
      null,
      "a cfg twice",
    ],
    [
      table(entry("a", "f.cpp", "x").replace(/ +marker.*\n/, "")),
      null,
      "an entry with no marker",
    ],
    [
      table(entry("a", "f.cpp", "x").replace('"x"', "MARKER")),
      null,
      "a marker that is no literal",
    ],
  ]) {
    const read = skiaPatchTable(rust);
    if (JSON.stringify(read.entries ?? null) !== JSON.stringify(expected)) {
      console.error(
        `check-patched-skia self-test: ${what} read as ${JSON.stringify(read.entries ?? read.error)}`,
      );
      process.exit(1);
    }
    count += 1;
  }
  // The real table, with an entry for every patch read from it.
  const real = skiaPatchTable(readFileSync(join(ROOT, "build.rs"), "utf8"));
  for (const cfg of [
    "meo_atlas_size",
    "meo_triangulation_tolerance",
    "meo_async_transfer_reuse",
  ])
    if (!real.entries?.[cfg]) {
      console.error(`check-patched-skia self-test: build.rs has no ${cfg}`);
      process.exit(1);
    }
  try {
    for (const [platform, patched] of Object.entries(PATCHED)) {
      for (const [files, text, accepted, what] of cases(root, patched)) {
        for (const [file, contents] of Object.entries(files)) {
          const path = join(root, "skia", file);
          mkdirSync(join(path, ".."), { recursive: true });
          writeFileSync(path, contents);
        }
        if ((refusal(text, patched) === null) !== accepted) {
          console.error(
            `check-patched-skia self-test: ${platform}, ${what} was ${accepted ? "refused" : "accepted"}`,
          );
          process.exit(1);
        }
        count += 1;
      }
      // samurai's build as cargo files it: what it compiled in `output`, the
      // directory it ran in in `stderr`. Read through `buildLog`, as `check`
      // reads it, with and without the `stderr`.
      const [files, text] = cases(root, patched).find(
        ([, , , what]) => what === "samurai run as ninja",
      );
      for (const [file, contents] of Object.entries(files))
        writeFileSync(join(root, "skia", file), contents);
      const lines = text.split("\n");
      const ninjaLine = lines.find((line) =>
        line.includes(": entering directory"),
      );
      const build = join(root, "build");
      mkdirSync(build, { recursive: true });
      writeFileSync(
        join(build, "output"),
        lines.filter((line) => line !== ninjaLine).join("\n"),
      );
      for (const [stderr, accepted] of [
        [ninjaLine, true],
        [null, false],
      ]) {
        rmSync(join(build, "stderr"), { force: true });
        if (stderr !== null) writeFileSync(join(build, "stderr"), stderr);
        if ((refusal(buildLog(build), patched) === null) !== accepted) {
          console.error(
            `check-patched-skia self-test: ${platform}, samurai's directory line ${stderr === null ? "missing from" : "in"} stderr was ${accepted ? "refused" : "accepted"}`,
          );
          process.exit(1);
        }
        count += 1;
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  console.log(
    `check-patched-skia: self-test, ${count} cases, each unpatched shape refused`,
  );
}

// Run as a script only: `scripts/check-skia-patches.mjs` imports `PATCHED`
// and `UNVERIFIED`.
const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
const arg = process.argv[2];
if (!main);
else if (arg === "--self-test") selfTest();
else if (arg) check(arg);
else {
  console.error(
    "usage: node scripts/check-patched-skia.mjs <target/<profile>/build> | --self-test",
  );
  process.exit(2);
}
