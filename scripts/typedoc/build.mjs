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
// escape sequences around them.
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
      "finding(s) above. These are defects in lib/*.d.ts — a link that " +
      "resolves to nothing, or a type used in a signature without being " +
      "exported — and a reader hits them as dead ends.",
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
