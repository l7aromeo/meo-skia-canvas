//
// Fails when changelog prose states a number of entries that the entries
// themselves contradict.
//
// The unreleased sections open with a paragraph that counts what follows --
// "Seven entries below break", and in the npm file a cross-reference reading
// "five of that file's seven breaking entries". Both are claims about a list
// that grows underneath them, and both went stale three times in one day:
// "three of the entries below break the crate's public API" survived a split
// that moved those entries into another file; "the crate breaks in three
// places of its own" was written to replace it and inherited the same wrong
// number; and "Six entries below break" was correct until a Breaking entry
// was appended half an hour later.
//
// Nothing else catches it. Prettier is satisfied, the prose reads as
// deliberate, and a reviewer counting seven bullet points has no reason to
// scroll back to a paragraph they already read. The number is only checkable
// against something a hundred lines away, which is exactly the distance at
// which people stop checking.
//
// WHY A COUNT AND NOT THE WHOLE CLAIM. This checks arithmetic, not truth. It
// cannot tell whether an entry belongs under Breaking, whether a change
// really reaches both surfaces, or whether the sentence describes the right
// file -- all three have been wrong here too, and all three need reading.
// What it does remove is the failure that recurs without anyone being
// careless: a number that was right when written.
//
// Only the UNRELEASED block is checked. Released sections are history and
// their counts describe what shipped.
//
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const WORDS = {
  one: 1,
  two: 2,
  three: 3,
  four: 4,
  five: 5,
  six: 6,
  seven: 7,
  eight: 8,
  nine: 9,
  ten: 10,
  eleven: 11,
  twelve: 12,
  thirteen: 13,
  fourteen: 14,
  fifteen: 15,
  sixteen: 16,
  seventeen: 17,
  eighteen: 18,
  nineteen: 19,
  twenty: 20,
};

const numeral = (token) => {
  const word = WORDS[token.toLowerCase()];
  return word ?? (/^\d+$/.test(token) ? Number(token) : null);
};

/** The UNRELEASED block of a changelog, or "" when it has none. */
function unreleased(text) {
  const lines = text.split("\n");
  const start = lines.findIndex((l) => /^## .*UNRELEASED/.test(l));
  if (start === -1) return "";
  const rest = lines.slice(start + 1).findIndex((l) => /^## /.test(l));
  return lines
    .slice(start, rest === -1 ? undefined : start + 1 + rest)
    .join("\n");
}

/** Entries -- lines opening `- **` -- under a `### <name>` heading. */
function entriesUnder(block, section) {
  let inside = false;
  let count = 0;
  for (const line of block.split("\n")) {
    if (/^### /.test(line)) inside = line.slice(4).trim() === section;
    else if (inside && /^- \*\*/.test(line)) count += 1;
  }
  return count;
}

// The prose is free-form, so each pattern is anchored on wording specific
// enough that a match is certainly a claim about a count. A sentence no
// pattern matches is not checked: the gate catches drift in the claims it
// knows, and does not constrain how prose may be phrased.
//
// ONCE A PATTERN MATCHES, THOUGH, THE CLAIM IS CHECKED OR THE RUN FAILS.
// Those are different situations and used to share an outcome. An unmatched
// sentence is prose this was never asked about; a matched one whose number
// cannot be read is a claim that has been recognised as a claim, and letting
// it through leaves a sentence that looks checked, is not, and reports green.
// "Ten" against eleven entries failed and "Ninety" passed, purely because the
// table below stops at twenty.
const CLAIMS = [
  {
    // "Seven entries below break", and "six of the entries below break" --
    // both about this file's own Breaking list. The optional half is not
    // decoration: without it the capture group takes `the`, which is not a
    // number, and the claim went to the branch that used to skip silently.
    // The npm file has been phrased that way and unchecked for as long as
    // this file has existed.
    pattern: /(\w+)(?: of the)? entries below break/gi,
    subject: "self",
  },
  {
    // "five of that file's seven breaking entries". The second number is the
    // *other* file's Breaking list, which is the half that drifts; the first
    // is a subset of it, checked as far as arithmetic reaches -- see
    // `checkSubset`.
    pattern: /(\w+) of that file's (\w+) breaking entries/gi,
    subject: "other",
  },
];

// THE SUBSET HALF OF A CROSS-REFERENCE, AND WHY IT IS ONLY HALF.
//
// "six of that file's eleven breaking entries are these same changes seen
// from Rust" carries two claims. The second number is the other file's
// Breaking total, and it is checked exactly, because it counts a list this
// can count. The first is a claim about *which* of those entries are the same
// change on the other channel, and nothing here can read that: it needs
// someone to decide whether a Rust entry and an npm entry describe one change
// or two, which is the judgement the note at the top of this file says this
// gate does not make.
//
// What is arithmetic is that a subset cannot be larger than the set it is
// drawn from, so that is what this checks. It catches the drift that happens
// -- the other file loses entries while the sentence keeps its number -- and
// it leaves "six of eleven" unverified in the direction that needs reading.
// Written down rather than left as an absence, so the next reader can tell a
// decision from an oversight.
function checkSubset(name, quoted, token, actual, target) {
  const sub = numeral(token);
  if (sub === null)
    return [
      `${name}: ${quoted} states "${token}", which is not a number this ` +
        `reads, so the subset it names goes unchecked.`,
    ];
  if (sub > actual)
    return [
      `${name}: ${quoted} draws ${sub} entries from ${target}'s Breaking ` +
        `list, which holds ${actual}`,
    ];
  return [];
}

function check(files) {
  const problems = [];
  const blocks = new Map();
  for (const [name, text] of files) blocks.set(name, unreleased(text));

  for (const [name, block] of blocks) {
    // Prose wraps at ~78 columns, so a claim is regularly split across two
    // lines. Fold before matching or every multi-line claim reads as absent.
    const folded = block.replace(/\s+/g, " ");
    // Two files, so "the other" is unambiguous. A third would make this
    // pick whichever came first; the caller passes only the channel files.
    const other = [...blocks.keys()].find((k) => k !== name);

    for (const claim of CLAIMS) {
      for (const found of folded.matchAll(claim.pattern)) {
        const target = claim.subject === "self" ? name : other;
        if (!target) continue;
        const quoted = `"${found[0].trim()}"`;
        const actual = entriesUnder(blocks.get(target), "Breaking");
        const token = claim.subject === "self" ? found[1] : found[2];
        const stated = numeral(token);

        // A word the table does not hold used to be skipped here, and that
        // is the failure this gate exists to prevent, one level up: the
        // sentence still reads as a claim, nothing checks it, and the run is
        // green. "Ten" against eleven entries was caught and "Ninety" was
        // not, which is the wrong way round -- the second is the one nobody
        // wrote on purpose.
        if (stated === null) {
          problems.push(
            `${name}: ${quoted} states "${token}", which is not a number ` +
              `this reads, so the claim goes unchecked. Use a digit or a ` +
              `number word up to twenty.`,
          );
        } else if (stated !== actual) {
          problems.push(
            `${name}: ${quoted} but ${target} has ${actual} entries under ` +
              `Breaking`,
          );
        }

        if (claim.subject === "other") {
          problems.push(...checkSubset(name, quoted, found[1], actual, target));
        }
      }
    }
  }
  return problems;
}

// THE INDEX'S OWN COUNTS. `CHANGELOG.md` has no UNRELEASED block, so the
// check above skips it entirely -- adding it to the path list would pass
// vacuously and read as coverage. What it does carry is a table counting the
// released sections in the other two files, and every number in it was wrong
// once: it routed 23 crate releases to a file that held none. So this counts
// the `## 📦` headings for real and compares.
function checkIndexTable(files) {
  const problems = [];
  const byName = new Map(files);
  const index = byName.get("CHANGELOG.md");
  if (index === undefined) return problems;

  const released = (text) =>
    text
      .split("\n")
      .filter((l) => l.startsWith("## \u{1F4E6}") && !l.includes("UNRELEASED"))
      .length;

  // A table row naming a changelog file and a number: the link text is the
  // file, so a renamed file stops matching rather than matching wrongly.
  const row = /^\|.*\[(CHANGELOG-[a-z]+\.md)\].*\|\s*(\d+)\s*\|/gm;
  for (const m of index.matchAll(row)) {
    const [, name, claimed] = m;
    const body = byName.get(name);
    if (body === undefined) continue;
    const actual = released(body);
    if (Number(claimed) !== actual) {
      problems.push(
        `CHANGELOG.md says ${name} holds ${claimed} released sections; it holds ${actual}`,
      );
    }
  }
  return problems;
}

// THE TAGS, WHICH ARE NOT DERIVED FROM THE CHANGELOG.
//
// The check above compares the index's table against the sections it counts,
// and both were wrong together: the table said the crate file held 20
// released sections and it held 20, so the comparison agreed and proved
// nothing. Four crate releases had been dropped when the history was split by
// channel -- the crate file was built from the dual-channel releases, and a
// crate-only release is by definition not one of those -- and the numbers
// moved together because one was written from the other.
//
// A tag is the record of what shipped and nobody derives it from a changelog,
// so it is the reference the comparison was missing. `v3.7.0` is the proof
// that this is worth having: it is a real npm release with its own
// `package.json` bump, it has no section in any file, and the only trace of
// it anywhere is the left-hand side of the `v4.0.0` compare link. No count
// would ever have found it.
//
// SORTED BY VERSION, NEVER BY DEFAULT. `git tag | tail` calls `rust-v0.9.1`
// the newest tag in this repository and sorts `0.13.0` before `0.9.0`. That
// has cost time here before, so the sort is explicit even where only
// membership is tested.
const RELEASE = /^(\d+)\.(\d+)\.(\d+)$/;

// Versions that shipped and have no section, each with a reason a reader can
// check without trusting this list.
//
// EVERY ONE IS AN ANCESTOR OF `v3.6.0`, the fork point the index names when
// it says the npm file continues `phyron-skia-canvas` from `3.6.0`. So the
// reason is testable in one command --
//
//     git merge-base --is-ancestor v3.4.4 v3.6.0
//
// -- and they were tagged by the upstream maintainers between August 2020 and
// April 2026, where this project's own releases begin in May 2026. They are
// inherited history whose changelog the upstream project never wrote.
//
// Enumerated rather than expressed as "anything before 3.6.0", because that
// rule would also swallow the next release that goes missing under the fork
// point, and an exception list is where a real gap goes to be silenced.
//
// `3.7.0` is deliberately NOT here. It is this project's own release, tagged
// the same day as the 3.6.0 and 4.0.0 either side of it, and its absence is a
// finding rather than a decision.
//
// `1.1.0` is deliberately NOT here either, and it was until an audit of this
// list: it is not an ancestor of anything in this tree, so it is covered by
// the reachability rule below rather than by a reason that is false for it.
// Thirteen entries where there were fourteen, and the one that moved was the
// one whose stated reason did not hold.
const WITHOUT_A_SECTION = new Map(
  [
    "0.9.15",
    "0.9.16",
    "0.9.17",
    "0.9.18",
    "3.1.0",
    "3.2.0",
    "3.2.1",
    "3.2.2",
    "3.4.0",
    "3.4.1",
    "3.4.2",
    "3.4.3",
    "3.4.4",
  ].map((v) => [
    `npm ${v}`,
    "inherited history: an ancestor of the v3.6.0 fork point, tagged upstream",
  ]),
);

/** Every tag matching any of `patterns`, newest first, by version not string. */
function tags(patterns) {
  const out = execFileSync(
    "git",
    ["tag", "--list", ...patterns, "--sort=-v:refname"],
    { encoding: "utf8" },
  );
  return out.split("\n").filter(Boolean);
}

// A TAG THAT IS NOT REACHABLE FROM HEAD IS NOT A RELEASE OF THIS TREE. Four
// are not -- the `1.1.x` family, tagged upstream in 2024 on history this fork
// does not contain -- and one of them, `1.1.0`, is a plain version that would
// otherwise be reported as a missing section for ever.
//
// A rule rather than four more list entries, because it is checkable in one
// command and needs no edit when the next orphan appears:
//
//     git merge-base --is-ancestor v1.1.0 HEAD
//
// Narrow by measurement: across every tag in the repository it excuses
// exactly one plain version.
// A shallow clone carries tags but almost no history, so `git tag --merged`
// reports nearly nothing reachable. That is indistinguishable from "every tag
// has a section" at the point where it is consumed, so it is refused here
// rather than skipped below. `actions/checkout` defaults to depth 1 and would
// otherwise turn this gate into a green light on every run.
function isShallow() {
  const out = execFileSync("git", ["rev-parse", "--is-shallow-repository"], {
    encoding: "utf8",
  });
  return out.trim() === "true";
}

function reachableTags() {
  const out = execFileSync("git", ["tag", "--merged", "HEAD"], {
    encoding: "utf8",
  });
  return new Set(out.split("\n").filter(Boolean));
}

// A crate release is written into the heading beside its npm twin --
// `[v5.9.0] (npm) / [v0.15.0] (crate)` -- because most releases ship on both
// channels at once. So the crate side asks for the version marked `(crate)`
// rather than for the version alone, which would also match the npm number in
// the same heading.
//
// THE NPM CHANNEL HAS TWO TAG SHAPES AND KEEPS BOTH. Releases through 5.9.0
// are tagged `v5.9.0`; from 6.0.0 they are tagged `npm-v6.0.0`, so that an
// npm tag is told apart from a crate one by looking at it. The 113 existing
// bare tags are not renamed -- published packages point at them -- so both
// patterns stay, and `git tag --list` takes as many as it is given.
//
// Matching only the old shape would not have failed loudly. `npm-v6.0.0`
// starts with neither `v` nor `rust-v`, so it would not be enumerated, and a
// tag this never looks at cannot be reported as missing a section: the first
// release under the new scheme would have passed this gate by being invisible
// to it.
const CHANNELS = [
  {
    label: "crate",
    patterns: ["rust-v*"],
    strip: (t) => t.replace(/^rust-v/, ""),
    file: "CHANGELOG-crate.md",
    present: (text, v) => text.includes(`[v${v}] (crate)`),
  },
  {
    label: "npm",
    patterns: ["v*", "npm-v*"],
    strip: (t) => t.replace(/^(?:npm-)?v/, ""),
    file: "CHANGELOG-npm.md",
    present: (text, v) =>
      new RegExp(`^## .*\\[v${v.replace(/\./g, "\\.")}\\]`, "m").test(text),
  },
];

// `listTags` is a parameter so the self-test can hand it a fixed list. The
// real run reads the repository, and a check whose only exercise is the
// repository's current state is one that stops being exercised the moment
// that state is fixed.
function checkTagsHaveSections(
  files,
  listTags = tags,
  reachable = reachableTags,
  shallow = isShallow,
) {
  const problems = [];
  const byName = new Map(files);
  // Before anything is skipped for being unreachable, establish that
  // reachability means what it says here.
  if (shallow()) {
    return [
      "this is a shallow clone, so tag reachability cannot be determined and " +
        "every tag would be skipped as unreachable -- check out with " +
        "fetch-depth: 0 before running this",
    ];
  }
  const inThisTree = reachable();
  for (const channel of CHANNELS) {
    const body = byName.get(channel.file);
    if (body === undefined) continue;
    const found = listTags(channel.patterns);
    // No tags is not a pass. An instrument that cannot see its reference has
    // to say so, or a shallow clone turns this check into a green light.
    if (found.length === 0) {
      problems.push(
        `no ${channel.label} tags matched ${channel.patterns.map((p) => `'${p}'`).join(" or ")}, so ${channel.file} ` +
          `cannot be checked against what shipped -- fetch tags before running this`,
      );
      continue;
    }
    for (const tag of found) {
      const version = channel.strip(tag);
      // A prerelease is not a release the changelog documents, and saying so
      // by shape rather than by listing keeps the next `-rc.1` out of the
      // table below.
      //
      // THIS AGREES WITH THE RECIPE THAT CUTS THE TAG, and the agreement is
      // the point rather than a coincidence. `release-npm` and
      // `release-crate` both skip their changelog guard when the version
      // carries a `-`, because a prerelease exists to exercise the pipeline
      // and not to be read. A gate demanding a section for one would
      // contradict the recipe that created it, and a red on every `-rc` is
      // how a gate teaches people to ignore it.
      if (!RELEASE.test(version)) continue;
      if (!inThisTree.has(tag)) continue;
      if (channel.present(body, version)) continue;
      const excused = WITHOUT_A_SECTION.get(`${channel.label} ${version}`);
      if (excused !== undefined) continue;
      problems.push(
        `${tag} shipped and ${channel.file} has no section for it` +
          (excused === undefined ? "" : ` (${excused})`),
      );
    }
  }
  return problems;
}

// A self-test, because a checker that has never been shown to fire says
// nothing when it is quiet. Both directions: a tree it must pass and a tree
// it must fail, differing only in the number.
function selfTest() {
  const withCount = (n) => `## [UNRELEASED]

**Not decided.** ${n} entries below break, so this is not a patch.

### Breaking

- **One.** Body.
- **Two.** Body.

### Fixed

- **Not counted.** Under a different heading.
`;
  // The claim under test is the *second* number -- the other file's Breaking
  // count -- since that is the one that drifts when the other file grows.
  const referring = (total, sub = "two") => `## [UNRELEASED]

Two are ours; ${sub} of that file's ${total} breaking entries are shared.

### Breaking

- **Only one here.** Body.
`;
  const cases = [
    ["a correct count passes", [["a.md", withCount("Two")]], 0],
    ["a stale count fails", [["a.md", withCount("Six")]], 1],
    ["a digit works too", [["a.md", withCount("2")]], 0],
    [
      "a claim split across lines is still read",
      [["a.md", withCount("Two").replace("Two entries", "Two\nentries")]],
      0,
    ],
    [
      "a cross-reference to the other file is checked",
      [
        ["a.md", referring("Two")],
        ["b.md", withCount("Two")],
      ],
      0,
    ],
    [
      "a stale cross-reference fails",
      [
        ["a.md", referring("Nine")],
        ["b.md", withCount("Two")],
      ],
      1,
    ],
    [
      "a file with no UNRELEASED block is skipped",
      [["a.md", "## [1.0.0]\n\n### Breaking\n\n- **x.** y.\n"]],
      0,
    ],
    // The case this gate was blind to. `Ten` against two entries already
    // failed; `Ninety` passed, because the recogniser had no entry for it and
    // an unreadable claim was skipped rather than refused. The fourth field
    // is what separates the two outcomes: both produce one problem, and only
    // one of them names the word.
    [
      "a number word the recogniser does not know is refused",
      [["a.md", withCount("Ninety")]],
      1,
      "Ninety",
    ],
    // "six of the entries below break" is how the npm file has always been
    // phrased. The capture group took `the`, which is not a number, so the
    // claim went to the same skip -- unchecked for as long as it has existed.
    [
      "the 'N of the entries' phrasing is read",
      [["a.md", withCount("Two").replace("Two entries", "Two of the entries")]],
      0,
    ],
    [
      "and is checked, not merely matched",
      [["a.md", withCount("Six").replace("Six entries", "Six of the entries")]],
      1,
    ],
    // The subset half. Nine of a list of two is arithmetic, and wrong,
    // whatever the sentence means by "shared".
    [
      "a subset larger than the list it draws from fails",
      [
        ["a.md", referring("Two", "Nine")],
        ["b.md", withCount("Two")],
      ],
      1,
    ],
    [
      "an unreadable subset word is refused too",
      [
        ["a.md", referring("Two", "Ninety")],
        ["b.md", withCount("Two")],
      ],
      1,
      "Ninety",
    ],
  ];

  let bad = 0;

  // The index table, in both directions. Without the failing case the row
  // pattern could match nothing and every tree would look correct.
  const indexTable = (n) =>
    `# Changelog\n\n| file | released |\n| ---- | -------- |\n` +
    `| [CHANGELOG-crate.md](CHANGELOG-crate.md) | ${n} |\n`;
  const twoReleases =
    "## \u{1F4E6} \u27E9 [UNRELEASED]\n\n## \u{1F4E6} \u27E9 [v1.0.0]\n\n## \u{1F4E6} \u27E9 [v0.9.0]\n";
  const indexCases = [
    ["the index count is right", indexTable("2"), twoReleases, 0],
    ["the index count is stale", indexTable("23"), twoReleases, 1],
    ["UNRELEASED is not counted", indexTable("2"), twoReleases, 0],
  ];
  for (const [label, index, crate, expected] of indexCases) {
    const got = checkIndexTable([
      ["CHANGELOG.md", index],
      ["CHANGELOG-crate.md", crate],
    ]).length;
    if (got !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got}`,
      );
      bad += 1;
    }
  }

  // A stub that FILTERS BY THE PATTERN, because one that does not cannot see
  // the pattern at all. This used to hand the npm fixtures over whenever the
  // channel was not the crate one, so every tag case tested what happens
  // after enumeration and none tested what is enumerated -- and narrowing the
  // npm channel back to `v*` alone left the suite green, which is how that
  // was found. Now the same edit turns two cases red.
  const listFrom = (all) => (patterns) =>
    all.filter((tag) =>
      patterns.some((glob) =>
        new RegExp(
          `^${glob.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`,
        ).test(tag),
      ),
    );

  // The tag check, on a fixed tag list rather than the repository's own: the
  // point is the rule, and a case reading real tags would change meaning
  // every time one is cut.
  const crateFile = ["CHANGELOG-crate.md", "## 📦 ⟩ [v9.9.9] (crate) ⟩ today"];
  const npmFile = ["CHANGELOG-npm.md", "## 📦 ⟩ [v8.8.8] ⟩ today"];
  // Each case names the tags it offers AND which of them this tree contains,
  // so reachability is exercised rather than read from the repository. The
  // parameter defaults to the real repository, and a case that let it do so
  // stopped testing anything the moment a fixture tag was not a real tag --
  // which is how the fourth case below started passing for the wrong reason.
  const tagCases = [
    [
      "a tagged release with a section passes",
      ["rust-v9.9.9"],
      ["v8.8.8"],
      null,
      0,
    ],
    [
      "a tagged release with no section is named",
      ["rust-v9.9.8"],
      ["v8.8.8"],
      null,
      1,
    ],
    [
      "a prerelease with no section is not",
      ["rust-v9.9.8-rc.1"],
      ["v8.8.8"],
      null,
      0,
    ],
    ["an excused version is not named", ["rust-v9.9.9"], ["v3.4.4"], null, 0],
    ["no tags at all is a failure, not a pass", [], [], null, 2],
    // The shallow case, both ways. Tags are present and plentiful, and the
    // clone is too shallow for `--merged` to place any of them: without the
    // guard every tag is skipped as unreachable and the run is a silent pass,
    // so the second row here is the one that would have shipped green.
    [
      "a shallow clone is refused rather than skipped",
      ["rust-v9.9.8"],
      ["v8.8.8"],
      [],
      1,
      true,
    ],
    [
      "the same tree passes nothing silently when it is not shallow",
      ["rust-v9.9.8"],
      ["v8.8.8"],
      [],
      0,
      false,
    ],
    // The reachability rule, both ways: one missing section, excused when the
    // tag is not in this tree and reported once it is.
    [
      "a tag this tree does not contain is not named",
      ["rust-v9.9.9"],
      ["v8.8.8", "v7.7.7"],
      ["rust-v9.9.9", "v8.8.8"],
      0,
    ],
    [
      "the same tag IS named once this tree contains it",
      ["rust-v9.9.9"],
      ["v8.8.8", "v7.7.7"],
      ["rust-v9.9.9", "v8.8.8", "v7.7.7"],
      1,
    ],
    // The new npm tag shape. Both shapes at once, which is what the
    // repository holds from 6.0.0 on: the old tag has its section and the new
    // one does not, so exactly one is reported.
    //
    // Offering `npm-v9.9.7` ALONE would look like a sharper case and is not
    // one. Narrow the channel back to `v*` and that fixture filters to
    // nothing, which trips the "no npm tags matched" branch -- also one
    // problem, so the case passes while the pattern it tests is wrong. The
    // bare tag beside it is what keeps the list non-empty, so the one problem
    // can only be the new tag.
    //
    // Red under both halves of the change: narrowing the patterns to `v*`,
    // and leaving the `npm-` prefix on in `strip`, each make this fail.
    [
      "the two npm tag shapes are enumerated together",
      ["rust-v9.9.9"],
      ["v8.8.8", "npm-v9.9.7"],
      null,
      1,
    ],
  ];
  for (const [
    label,
    crateTags,
    npmTags,
    contained,
    expected,
    shallow = false,
  ] of tagCases) {
    // Every dependency is injected, including this one. A parameter left to
    // read the live repository makes a case pass for a reason the case does
    // not state -- which has happened here once already.
    const got = checkTagsHaveSections(
      [crateFile, npmFile],
      listFrom([...crateTags, ...npmTags]),
      () => new Set(contained ?? [...crateTags, ...npmTags]),
      () => shallow,
    ).length;
    if (got !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got}`,
      );
      bad += 1;
    }
  }

  for (const [label, files, expected, mustSay] of cases) {
    const got = check(files);
    if (got.length !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got.length}`,
      );
      bad += 1;
    } else if (mustSay && !got.some((p) => p.includes(mustSay))) {
      // A refusal and a stale count both produce one problem, so a count
      // alone cannot tell them apart -- and the refusal is the new half.
      console.error(
        `  self-test FAILED: ${label} -- no problem mentioned "${mustSay}"`,
      );
      bad += 1;
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `self-test: ${cases.length + indexCases.length + tagCases.length} cases, a ` +
      `stale count is caught in the direct, the cross-referencing and the ` +
      `index-table form, an unreadable number word is refused rather than ` +
      `skipped, and a shipped tag with no section is named under either npm ` +
      `tag shape`,
  );
}

const args = process.argv.slice(2);
if (args.includes("--self-test")) {
  selfTest();
} else {
  const paths =
    args.length > 0
      ? args
      : ["CHANGELOG.md", "CHANGELOG-crate.md", "CHANGELOG-npm.md"];
  const loaded = paths.map((p) => [p, readFileSync(p, "utf8")]);
  // `check` resolves "that file" as the one that is not this one, so it takes
  // the two channel files and nothing else. Handing it the index as a third
  // made the npm file's cross-reference resolve against `CHANGELOG.md`, which
  // has no Breaking list, and the run failed on a claim that was correct.
  const channels = loaded.filter(([p]) => p !== "CHANGELOG.md");
  const problems = [
    ...check(channels),
    ...checkIndexTable(loaded),
    ...checkTagsHaveSections(loaded),
  ];
  if (problems.length > 0) {
    console.error("changelog prose disagrees with the entries it counts:\n");
    for (const p of problems) console.error(`  ${p}`);
    process.exit(1);
  }
  console.log(`changelog counts agree with the entries (${paths.join(", ")})`);
}
