#!/usr/bin/env node
// `PATCHES` in scripts/skia-source.mjs is the one list of each platform's
// Skia patches, and nothing else carries a copy of it.
//
//   node scripts/check-skia-patches.mjs
//   node scripts/check-skia-patches.mjs --self-test
//
// A copy builds green when it is short of a patch: #553's patch went into
// `PATCHES` and into none of the Linux release build's own patch step and
// the cache keys' `hashFiles` lists, so the published Linux binary would have
// gone without it. CI now asks the script instead -- `skia-source.mjs apply`
// patches the release build's checkout and `skia-source.mjs cache-key` names
// each cache -- and this refuses what would bring a copy back:
//
// - A workflow or action that names a `containers/*.patch` file anywhere
//   but a comment, or a Skia source cache whose key is not the output of a
//   `cache-key` step before it, or a release patch step that does not run
//   `apply`.
// - A patch a platform takes that `scripts/check-patched-skia.mjs` neither
//   has a marker for nor names in `UNVERIFIED`, with why.
// - A patch a platform neither takes nor withholds, with the reason, in
//   `WITHHELD`, so going without one is a decision on the page and never an
//   omission.
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";

import { PATCHED, UNVERIFIED } from "./check-patched-skia.mjs";
import { PATCHES, WITHHELD } from "./skia-source.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// Where workflows and actions live; every YAML file under it is read.
const GITHUB = ".github";

/// The file holding the Linux release build, the job that builds it, and the
/// step that patches its rust-skia checkout.
const RELEASE = {
  file: ".github/workflows/build.yml",
  job: "linux",
  step: "Checkout and patch rust-skia",
};

/// A patch file's name, as a workflow would spell it to apply or hash it.
const PATCH_FILE = /containers\/[\w.*-]+\.patch/g;

/// What a Skia source cache keeps: the cache directory skia-source names.
const SKIA_CACHE = "meo-skia-canvas";

/// A key taken from an earlier step's output, capturing the step's id.
const STEP_KEY = /^\$\{\{\s*steps\.([\w-]+)\.outputs\.key\s*\}\}$/;

const posix = (path) => path.replaceAll("\\", "/");

/**
 * Patches a platform neither takes nor withholds, withholds and takes, or
 * withholds without a reason, or withholds without any platform taking it.
 */
export function auditDecisions(patches, withheld) {
  const problems = [];
  const taken = new Set(Object.values(patches).flat().map(posix));
  const platforms = new Set([
    ...Object.keys(patches),
    ...Object.keys(withheld),
  ]);
  for (const platform of platforms) {
    const takes = new Set((patches[platform] ?? []).map(posix));
    const reasons = Object.fromEntries(
      Object.entries(withheld[platform] ?? {}).map(([p, why]) => [
        posix(p),
        why,
      ]),
    );
    for (const patch of taken) {
      if (takes.has(patch) && patch in reasons)
        problems.push(`${platform} both takes and withholds \`${patch}\``);
      else if (!takes.has(patch) && !(patch in reasons))
        problems.push(
          `${platform} neither takes nor withholds \`${patch}\`: add it to \`PATCHES.${platform}\`, or say why not in \`WITHHELD.${platform}\``,
        );
    }
    for (const [patch, why] of Object.entries(reasons)) {
      if (!taken.has(patch))
        problems.push(
          `${platform} withholds \`${patch}\`, which no platform takes`,
        );
      else if (typeof why !== "string" || why.trim() === "")
        problems.push(`${platform} withholds \`${patch}\` without a reason`);
    }
  }
  return problems;
}

/**
 * Patches a platform takes that `check-patched-skia` neither verifies nor
 * names as unverifiable, and markers for patches it does not take.
 */
export function auditVerified(patches, patched, unverified) {
  const problems = [];
  for (const [platform, takes] of Object.entries(patches)) {
    const markers = patched[platform];
    if (!markers) {
      problems.push(
        `check-patched-skia has no \`PATCHED.${platform}\`, so a ${platform} release build is never checked`,
      );
      continue;
    }
    const marked = new Set(markers.map(({ patch }) => posix(patch)));
    for (const patch of takes.map(posix))
      if (!marked.has(patch) && !(patch in unverified))
        problems.push(
          `check-patched-skia has no marker for \`${patch}\` on ${platform}, and \`UNVERIFIED\` does not name it`,
        );
    for (const patch of marked)
      if (!takes.map(posix).includes(patch))
        problems.push(
          `check-patched-skia checks ${platform} for \`${patch}\`, which \`PATCHES.${platform}\` does not take`,
        );
  }
  for (const platform of Object.keys(patched))
    if (!(platform in patches))
      problems.push(
        `check-patched-skia checks ${platform}, which \`PATCHES\` gives no tree`,
      );
  return problems;
}

/** Every string a parsed YAML document holds, comments being gone. */
function strings(node) {
  if (typeof node === "string") return [node];
  if (node === null || typeof node !== "object") return [];
  return Object.values(node).flatMap(strings);
}

/** Each list of steps in a workflow's jobs or a composite action. */
function stepLists(doc) {
  return [
    ...Object.values(doc?.jobs ?? {}).map((job) => job?.steps),
    doc?.runs?.steps,
  ].filter(Array.isArray);
}

/**
 * Patch files a workflow or action names, and Skia source caches whose key
 * is not the output of a `skia-source.mjs cache-key` step before them.
 */
export function auditWorkflow(file, text) {
  const doc = parse(text);
  const named = new Set(strings(doc).flatMap((s) => s.match(PATCH_FILE) ?? []));
  const problems = [...named].map(
    (patch) =>
      `${file} names \`${patch}\`: take the list from \`skia-source.mjs apply\` or \`cache-key\` instead`,
  );
  for (const steps of stepLists(doc))
    steps.forEach((step, i) => {
      const cache = /^actions\/cache(\/restore|\/save)?@/.test(
        step?.uses ?? "",
      );
      if (!cache || !String(step?.with?.path ?? "").includes(SKIA_CACHE))
        return;
      const id = String(step.with.key ?? "").match(STEP_KEY)?.[1];
      const source = steps
        .slice(0, i)
        .find((earlier) => earlier?.id === id && id !== undefined);
      if (!source || !/skia-source\.mjs cache-key/.test(source.run ?? ""))
        problems.push(
          `${file}'s "${step.name ?? step.uses}" keys the Skia source by \`${step.with.key}\`, not by the output of a \`skia-source.mjs cache-key\` step before it`,
        );
    });
  return problems;
}

/** The Linux release build's patch step, which has to run `apply`. */
export function auditRelease(text) {
  const steps = parse(text)?.jobs?.[RELEASE.job]?.steps ?? [];
  const run = steps.find((step) => step?.name === RELEASE.step)?.run;
  if (typeof run !== "string")
    return [
      `${RELEASE.file} has no "${RELEASE.step}" step in its \`${RELEASE.job}\` job to check`,
    ];
  if (/skia-source\.mjs apply /.test(run)) return [];
  return [
    `${RELEASE.file}'s "${RELEASE.step}" does not patch its checkout with \`skia-source.mjs apply\``,
  ];
}

/** The YAML files under `.github` at `root`, relative to it. */
function workflowFiles(root) {
  return readdirSync(join(root, GITHUB), { recursive: true })
    .map((path) => posix(join(GITHUB, path)))
    .filter((path) => /\.ya?ml$/.test(path))
    .sort();
}

/** Every problem in the tree at `root`. */
export function audit(root = ROOT) {
  const read = (file) => readFileSync(join(root, file), "utf8");
  return [
    ...auditDecisions(PATCHES, WITHHELD),
    ...auditVerified(PATCHES, PATCHED, UNVERIFIED),
    ...auditRelease(read(RELEASE.file)),
    ...workflowFiles(root).flatMap((file) => auditWorkflow(file, read(file))),
  ];
}

function selfTest() {
  let failed = 0;
  const expect = (what, problems, count) => {
    if (problems.length !== count) {
      console.error(
        `self-test "${what}": want ${count} problems, got ${JSON.stringify(problems)}`,
      );
      failed += 1;
    }
  };

  const patches = { darwin: ["m", "a", "x"], linux: ["b", "a", "x"] };
  const withheld = {
    darwin: { b: "Linux only" },
    linux: { m: "Metal only" },
    win32: { a: "unpatched", x: "a crash", m: "Metal only", b: "Linux only" },
  };
  expect(
    "every platform decides every patch",
    auditDecisions(patches, withheld),
    0,
  );
  expect(
    "a patch one platform leaves undecided",
    auditDecisions(patches, { ...withheld, win32: { a: "u", m: "M", b: "L" } }),
    1,
  );
  expect(
    "a patch taken and withheld",
    auditDecisions(patches, { ...withheld, linux: { m: "M", x: "why" } }),
    1,
  );
  expect(
    "a patch withheld without a reason",
    auditDecisions(patches, { ...withheld, darwin: { b: " " } }),
    1,
  );
  expect(
    "a patch withheld that no platform takes",
    auditDecisions(patches, { ...withheld, darwin: { b: "L", gone: "old" } }),
    1,
  );

  const marker = (patch) => ({ patch, file: `${patch}.cpp`, marker: patch });
  const patched = {
    darwin: [marker("m"), marker("a"), marker("x")],
    linux: [marker("a"), marker("x")],
  };
  const unverified = { b: "a build list" };
  expect(
    "every patch marked or unverified",
    auditVerified(patches, patched, unverified),
    0,
  );
  expect(
    "a patch with no marker",
    auditVerified(patches, { ...patched, linux: [marker("a")] }, unverified),
    1,
  );
  expect(
    "a patch neither marked nor unverified",
    auditVerified(patches, patched, {}),
    1,
  );
  expect(
    "a marker for a patch not taken",
    auditVerified(
      patches,
      { ...patched, linux: [...patched.linux, marker("m")] },
      unverified,
    ),
    1,
  );
  expect(
    "a platform with no markers",
    auditVerified({ ...patches, win32: ["a"] }, patched, unverified),
    1,
  );
  expect(
    "markers for a platform with no tree",
    auditVerified(patches, { ...patched, win32: [] }, unverified),
    1,
  );

  const keyStep = [
    "      - name: Name the key",
    "        id: skia-key",
    "        run: |",
    "          key=$(node scripts/skia-source.mjs cache-key)",
    '          echo "key=$key" >> "$GITHUB_OUTPUT"',
  ];
  const cacheStep = (key, uses = "actions/cache@v6") => [
    "      - name: Cache the Skia source",
    `        uses: ${uses}`,
    "        with:",
    "          path: ~/.cache/meo-skia-canvas",
    `          key: ${key}`,
  ];
  const job = (...steps) =>
    ["jobs:", "  build:", "    steps:", ...steps.flat()].join("\n");
  const computed = "${{ steps.skia-key.outputs.key }}";
  expect(
    "a key from cache-key",
    auditWorkflow("w.yml", job(keyStep, cacheStep(computed))),
    0,
  );
  expect(
    "a restore keyed from cache-key",
    auditWorkflow(
      "w.yml",
      job(keyStep, cacheStep(computed, "actions/cache/restore@v6")),
    ),
    0,
  );
  expect(
    "a composite action keyed from cache-key",
    auditWorkflow(
      "a.yml",
      [
        "runs:",
        "  using: composite",
        "  steps:",
        ...keyStep,
        ...cacheStep(computed),
      ]
        .map((l) => l.replace(/^ {4}/, "  "))
        .join("\n"),
    ),
    0,
  );
  expect(
    "a key that hashes patch files",
    auditWorkflow(
      "w.yml",
      job(
        cacheStep(
          "skia-source-${{ hashFiles('Cargo.lock', 'containers/a.patch') }}",
        ),
      ),
    ),
    2,
  );
  expect(
    "a key read before the step that names it",
    auditWorkflow("w.yml", job(cacheStep(computed), keyStep)),
    1,
  );
  expect(
    "a key from a step that is not cache-key",
    auditWorkflow(
      "w.yml",
      job(
        keyStep.map((l) => l.replace("cache-key", "dir")),
        cacheStep(computed),
      ),
    ),
    1,
  );
  expect(
    "a constant key",
    auditWorkflow("w.yml", job(cacheStep("skia-source"))),
    1,
  );
  expect(
    "another cache keyed by hand",
    auditWorkflow(
      "w.yml",
      job(
        cacheStep(computed).map((l) => l.replace("meo-skia-canvas", "cargo")),
      ),
    ),
    0,
  );
  expect(
    "a run step that applies a patch file",
    auditWorkflow(
      "w.yml",
      job(["      - run: patch -p1 -i containers/skia-atlas-size.patch"]),
    ),
    1,
  );
  expect(
    "a patch named only in a comment",
    auditWorkflow(
      "w.yml",
      job(["      # containers/skia-atlas-size.patch", "      - run: true"]),
    ),
    0,
  );

  const release = (run) =>
    [
      "jobs:",
      `  ${RELEASE.job}:`,
      "    steps:",
      `      - name: ${RELEASE.step}`,
      "        run: |",
      `          ${run}`,
    ].join("\n");
  expect(
    "a release step that runs apply",
    auditRelease(
      release("node skia-canvas/scripts/skia-source.mjs apply rust-skia"),
    ),
    0,
  );
  expect(
    "a release step that patches by hand",
    auditRelease(release('patch -p1 -F0 -d rust-skia -i "$W/x.patch"')),
    1,
  );
  expect(
    "no release step",
    auditRelease("jobs:\n  linux:\n    steps: []\n"),
    1,
  );

  if (failed === 0)
    console.log(
      "check-skia-patches: self-test, every named patch file, hand-made key, hand-patched release, unmarked patch and undecided platform refused",
    );
  return failed;
}

const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (main && process.argv.includes("--self-test")) {
  process.exit(selfTest() === 0 ? 0 : 1);
} else if (main) {
  const problems = audit();
  for (const problem of problems)
    console.error(`check-skia-patches: ${problem}`);
  if (problems.length > 0) process.exit(1);
  console.log(
    `check-skia-patches: no workflow names a patch, every Skia cache is keyed by \`cache-key\`, the release build runs \`apply\`, and every patch is marked and decided on every platform`,
  );
}
