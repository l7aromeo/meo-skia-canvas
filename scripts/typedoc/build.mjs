// Builds the JavaScript API reference and gates on what it found.
//
// TypeDoc reports one severity for every validation, and the two kinds of
// finding here do not deserve the same treatment. A broken link or a type
// that escapes into a signature without being exported is a defect in the
// declarations, and the build should stop. A member with no doc comment is a
// gap, and there are hundreds -- failing on those today would teach everyone
// to pass a flag that turns the whole check off, which is how a gate dies.
//
// So: structural findings fail immediately, and the coverage number ratchets.
// It may go down and it may hold. It may not go up.

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const baselineFile = join(here, "undocumented-baseline.txt");

// TypeDoc's own wording, matched rather than parsed: the message is the only
// thing distinguishing a coverage warning from a structural one.
const UNDOCUMENTED = "does not have any documentation";

// The palette in theme.css is copied from docs/generate/brand.js, the script
// that draws the hero banners. Copied, because CSS cannot read a JavaScript
// object -- which left the README asserting the two agree with nothing holding
// them together, so a colour changed there would have gone quietly stale here.
//
// Compared as a set of values rather than name by name: the mapping from
// `THEMES.hero.tile` to `--brand-tile` is not mechanical, and a table of those
// pairs would be a third place to drift. Every colour brand.js draws with has
// to appear as some `--brand-*`. The reverse does not hold -- theme.css also
// carries the two link colours derived for contrast, which brand.js has no
// reason to know about.
const BRAND_SOURCE = join(here, "..", "..", "docs", "generate", "brand.js");
const THEME = join(here, "theme.css");

/// `#abc` and `#aabbcc` both, normalised to the long lowercase form so the two
/// files can disagree about spelling without failing.
function hexes(text, pattern = /#[0-9a-fA-F]{3}(?:[0-9a-fA-F]{3})?\b/g) {
  return (text.match(pattern) ?? []).map((hex) => {
    const body = hex.slice(1).toLowerCase();
    return body.length === 3
      ? `#${body[0]}${body[0]}${body[1]}${body[1]}${body[2]}${body[2]}`
      : `#${body}`;
  });
}

for (const file of [BRAND_SOURCE, THEME]) {
  if (existsSync(file)) continue;
  console.error(
    `The palette check cannot find ${file}. If the branding moved, point ` +
      "this at its new home -- do not delete the check, or theme.css goes " +
      "back to being a copy nothing compares.",
  );
  process.exit(1);
}

const drawn = new Set(hexes(readFileSync(BRAND_SOURCE, "utf8")));
const declared = new Set(
  hexes(readFileSync(THEME, "utf8"), /--brand-[a-z-]+:\s*(#[0-9a-fA-F]{3,6})/g)
    .map((match) => match.slice(match.indexOf("#")))
    .flatMap((hex) => hexes(hex)),
);
const drifted = [...drawn].filter((hex) => !declared.has(hex));

if (drifted.length > 0) {
  console.error(
    `The palette has drifted from docs/generate/brand.js. It draws with ` +
      `${drifted.join(", ")}, which no --brand-* in theme.css carries.\n\n` +
      "Copy the new value across, or if the divergence is deliberate, say so " +
      "where the palette is declared -- the reference is supposed to match " +
      "the banner at the top of the README.",
  );
  process.exit(1);
}

const typedoc = join(here, "node_modules", ".bin", "typedoc");
if (!existsSync(typedoc)) {
  console.error(
    "The reference tool is not installed. Run `npm install` in " +
      "scripts/typedoc, or use `just docs-js`, which does it for you.",
  );
  process.exit(1);
}

// Both streams, because TypeDoc writes its findings to stderr and its
// progress to stdout -- reading only what a command returns would have found
// nothing to report and called that a clean build. Which it did, once.
const run = spawnSync(typedoc, ["--options", join(here, "typedoc.json")], {
  cwd: here,
  encoding: "utf8",
});
const output = `${run.stdout ?? ""}${run.stderr ?? ""}`;
const failed = run.status !== 0;
process.stdout.write(output);

// Strip the colour codes so the matching below reads the words, not the
// escape sequences around them. The literal escape character is what an ANSI
// sequence opens with, so it belongs in the pattern.
// eslint-disable-next-line no-control-regex
const plain = output.replace(/\[[0-9;]*m/g, "");
const lines = plain.split("\n");

const structural = lines.filter(
  (line) =>
    (line.includes("[warning]") || line.includes("[error]")) &&
    !line.includes(UNDOCUMENTED) &&
    !/Found \d+ errors and \d+ warnings/.test(line),
);
const undocumented = lines.filter((line) => line.includes(UNDOCUMENTED)).length;

for (const line of structural) console.error(line);

if (failed || structural.length > 0) {
  console.error(
    `\nThe reference did not build cleanly: ${structural.length} structural ` +
      "finding(s) above. A reader hits every one of them as a dead end. " +
      "Most come from lib/*.d.ts — a link that resolves to nothing, or a " +
      "type used in a signature without being exported — but index.md, " +
      "which becomes the entry page, links into the API by symbol name and " +
      "fails the same way when one of those is renamed or removed.",
  );
  process.exit(1);
}

const baseline = existsSync(baselineFile)
  ? Number.parseInt(readFileSync(baselineFile, "utf8").trim(), 10)
  : Number.POSITIVE_INFINITY;

console.log(`\nReference built. Undocumented members: ${undocumented}.`);

if (undocumented > baseline) {
  console.error(
    `\nThat is ${undocumented - baseline} more than the baseline of ` +
      `${baseline}. Document what you added, or say here why it needs no ` +
      "documentation.\n\nThe list is above, one line per member.",
  );
  process.exit(1);
}

if (undocumented < baseline) {
  writeFileSync(baselineFile, `${undocumented}\n`);
  console.log(
    `Down from ${baseline}. Baseline lowered — commit ` +
      "scripts/typedoc/undocumented-baseline.txt with the change.",
  );
}
