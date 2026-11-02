// Fails when a changelog entry from npm 7.0.0 or crate 0.17.0 on is not one
// line linked to its issue or pull request.
//
// From those releases an entry is a bold title in the reader's terms and the
// links to what made it -- `- **A drop shadow keeps its blur's tail.**
// ([#454](https://github.com/l7aromeo/meo-skia-canvas/issues/454))` -- and a
// Breaking entry adds one indented line saying what to change. The detail
// lives in the pull request and the commit message, which do not go stale
// against the code the way a paragraph here does.
//
// A section's entries form one tight list: a blank line between two of them,
// or between an entry and its Breaking line, makes the list loose, which
// Markdown renders as paragraphs spaced apart.
//
// The shape is what this checks, not the words. A title that says nothing,
// or links the wrong pull request, passes; both need reading. What it
// removes is the drift back to paragraphs, which every fix that adds an
// entry is one keystroke from.
//
// Sections before those releases are history and keep the form they shipped
// in.
//
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

/** The first release of each channel whose entries take the one-line form. */
const FIRST = {
  "CHANGELOG-npm.md": [7, 0, 0],
  "CHANGELOG-crate.md": [0, 17, 0],
};

/** A link to an issue or pull request of this repository, as `[#N](url)`. */
const LINK =
  /\[#(\d+)\]\(https:\/\/github\.com\/l7aromeo\/meo-skia-canvas\/(?:issues|pull)\/\1\)/;

/** A release heading: `## 📦 ⟩ [v7.0.0] (npm) ⟩ Unreleased`. */
const RELEASE = /^## .*\[v(\d+)\.(\d+)\.(\d+)[^\]]*\]/;

const atLeast = (version, floor) => {
  for (let i = 0; i < 3; i++) {
    if (version[i] !== floor[i]) return version[i] > floor[i];
  }
  return true;
};

/** The problems with `text`, a changelog whose one-line form starts at `floor`. */
export function check(text, floor) {
  const problems = [];
  let checked = false;
  let section = null;
  let entry = null;
  /** The line of a blank seen since the last entry's text, if any. */
  let blank = null;
  const loose = () => {
    if (blank === null) return;
    problems.push(`line ${blank}: a blank line inside the ${section} list`);
    blank = null;
  };
  const close = () => {
    if (!entry) return;
    const { line, title, extra, breaking } = entry;
    if (![title, ...extra].some((text) => LINK.test(text))) {
      problems.push(`line ${line}: links no issue or pull request`);
    }
    if (breaking && extra.length !== 1) {
      problems.push(
        `line ${line}: a Breaking entry takes one line saying what to change, not ${extra.length}`,
      );
    }
    if (!breaking && extra.length > 0) {
      problems.push(`line ${line}: runs to ${extra.length + 1} lines`);
    }
    entry = null;
  };
  text.split("\n").forEach((raw, index) => {
    const line = index + 1;
    const release = raw.match(RELEASE);
    if (release) {
      close();
      blank = null;
      checked = atLeast(release.slice(1, 4).map(Number), floor);
      section = null;
      return;
    }
    if (!checked) return;
    if (raw.startsWith("### ")) {
      close();
      blank = null;
      section = raw.slice(4).trim();
      return;
    }
    if (section === null) return;
    if (raw.startsWith("- ")) {
      close();
      loose();
      if (!/^- \*\*.+\*\*/.test(raw)) {
        problems.push(`line ${line}: an entry opens with its bold title`);
      }
      entry = { line, title: raw, extra: [], breaking: section === "Breaking" };
      return;
    }
    if (raw.trim() === "") {
      if (entry) blank = line;
      return;
    }
    if (entry) {
      loose();
      entry.extra.push(raw);
    } else {
      problems.push(`line ${line}: prose under ${section} outside an entry`);
    }
  });
  close();
  return problems;
}

/** Cases that must be refused or passed, so a checker that stopped refusing shows. */
function selfTest() {
  const url = (n, kind = "pull") =>
    `[#${n}](https://github.com/l7aromeo/meo-skia-canvas/${kind}/${n})`;
  const page = (body, version = "7.0.0") =>
    `# Changelog\n\n## 📦 ⟩ [v${version}] (npm) ⟩ Unreleased\n\n${body}\n`;
  const floor = [7, 0, 0];
  const cases = [
    [
      "a linked one-liner",
      page(`### Fixed\n\n- **A fill is exact.** (${url(440)})`),
      0,
    ],
    [
      "an issue link",
      page(`### Fixed\n\n- **A fill is exact.** (${url(452, "issues")})`),
      0,
    ],
    [
      "two links",
      page(`### Fixed\n\n- **A fill is exact.** (${url(440)}, ${url(452)})`),
      0,
    ],
    [
      "a wrapped entry",
      page(
        `### Fixed\n\n- **A fill is exact.** It was not,\n  and now is. (${url(440)})`,
      ),
      1,
    ],
    ["no link", page(`### Fixed\n\n- **A fill is exact.**`), 1],
    ["a bare #N", page(`### Fixed\n\n- **A fill is exact.** (#440)`), 1],
    [
      "another repository's link",
      page(
        `### Fixed\n\n- **A fill is exact.** ([#440](https://github.com/samizdatco/skia-canvas/pull/440))`,
      ),
      1,
    ],
    [
      "a link whose number and URL disagree",
      page(
        `### Fixed\n\n- **A fill is exact.** ([#440](https://github.com/l7aromeo/meo-skia-canvas/pull/441))`,
      ),
      1,
    ],
    [
      "Breaking with its line",
      page(
        `### Breaking\n\n- **X is refused.** (${url(311)})\n  Spell it as the standard does.`,
      ),
      0,
    ],
    [
      "Breaking with none",
      page(`### Breaking\n\n- **X is refused.** (${url(311)})`),
      1,
    ],
    [
      "Breaking with two",
      page(
        `### Breaking\n\n- **X is refused.** (${url(311)})\n  Spell it.\n  As the standard does.`,
      ),
      1,
    ],
    ["no bold title", page(`### Fixed\n\n- A fill is exact. (${url(440)})`), 1],
    [
      "a tight list",
      page(
        `### Fixed\n\n- **A fill is exact.** (${url(440)})\n- **A stroke is exact.** (${url(447)})`,
      ),
      0,
    ],
    [
      "a loose list",
      page(
        `### Fixed\n\n- **A fill is exact.** (${url(440)})\n\n- **A stroke is exact.** (${url(447)})`,
      ),
      1,
    ],
    [
      "a blank before a Breaking line",
      page(
        `### Breaking\n\n- **X is refused.** (${url(311)})\n\n  Spell it as the standard does.`,
      ),
      1,
    ],
    [
      "two lists, a heading apart",
      page(
        `### Fixed\n\n- **A fill is exact.** (${url(440)})\n\n### Added\n\n- **A stroke.** (${url(447)})`,
      ),
      0,
    ],
    [
      "prose under a heading",
      page(`### Fixed\n\nSome words.\n\n- **A fill is exact.** (${url(440)})`),
      1,
    ],
    [
      "an older release in paragraphs",
      page(
        `### Fixed\n\n- **A fill is exact.** It was not,\n  and now is.`,
        "6.0.2",
      ),
      0,
    ],
    [
      "a release's opening paragraph",
      page(
        `A major release.\n\n### Fixed\n\n- **A fill is exact.** (${url(440)})`,
      ),
      0,
    ],
  ];
  const failed = cases.filter(
    ([, text, want]) => check(text, floor).length !== want,
  );
  for (const [name, text, want] of failed) {
    console.error(
      `self-test "${name}": want ${want} problems, got ${JSON.stringify(check(text, floor))}`,
    );
  }
  if (failed.length) process.exit(1);
  console.log(`self-test: ${cases.length} cases`);
}

const main = import.meta.url === pathToFileURL(process.argv[1]).href;
if (main && process.argv.includes("--self-test")) {
  selfTest();
} else if (main) {
  let failed = false;
  for (const [file, floor] of Object.entries(FIRST)) {
    const problems = check(readFileSync(file, "utf8"), floor);
    for (const problem of problems) console.error(`${file} ${problem}`);
    failed ||= problems.length > 0;
    console.log(
      `${file}: ${problems.length ? "refused" : "every entry from v" + floor.join(".") + " one line, linked"}`,
    );
  }
  if (failed) process.exit(1);
}
