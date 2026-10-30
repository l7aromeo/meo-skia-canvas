const assert = require("assert");
const { execFileSync, spawnSync } = require("child_process");
const { createHash } = require("crypto");
const {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");
const { pathToFileURL } = require("url");
const { describe, test, mock, beforeEach, afterEach } = require("node:test");

const SCRIPT = join(__dirname, "..", "..", "scripts", "skia-source.mjs");

// What a cold cache meets: googlesource answers a burst of fetches with 429s
// that clear within a minute, so each network step of `prepare` is tried
// again after a wait, and says which try failed and why.
describe("skia-source retries a fetch that fails", () => {
  let retry;
  let logged;
  beforeEach(async () => {
    // A URL, not a path: the ESM loader reads Windows' `C:` as a URL scheme.
    ({ retry } = await import(pathToFileURL(SCRIPT).href));
    logged = [];
    mock.method(console, "error", (line) => logged.push(line));
  });
  afterEach(() => mock.restoreAll());

  /** A step that throws `failures` times, then answers "done". */
  const flaky = (failures) => {
    let calls = 0;
    return {
      step: () => {
        calls += 1;
        if (calls <= failures) throw new Error(`HTTP 429 on call ${calls}`);
        return "done";
      },
      calls: () => calls,
    };
  };

  test("a step that succeeds is run once and waits for nothing", () => {
    const waits = [];
    const { step, calls } = flaky(0);
    assert.equal(
      retry("syncing", step, { wait: (s) => waits.push(s) }),
      "done",
    );
    assert.equal(calls(), 1);
    assert.deepEqual(waits, []);
    assert.deepEqual(logged, []);
  });

  test("a step that fails twice waits, says why, and succeeds", () => {
    const waits = [];
    const { step, calls } = flaky(2);
    assert.equal(
      retry("syncing", step, { wait: (s) => waits.push(s) }),
      "done",
    );
    assert.equal(calls(), 3);
    assert.deepEqual(waits, [15, 30]);
    assert.deepEqual(logged, [
      "skia-source: syncing failed on try 1 of 4: HTTP 429 on call 1; trying again in 15 s",
      "skia-source: syncing failed on try 2 of 4: HTTP 429 on call 2; trying again in 30 s",
    ]);
  });

  test("a step that keeps failing stops with its own last error", () => {
    const waits = [];
    const { step, calls } = flaky(Infinity);
    assert.throws(
      () => retry("syncing", step, { wait: (s) => waits.push(s) }),
      /HTTP 429 on call 4/,
    );
    assert.equal(calls(), 4);
    assert.deepEqual(waits, [15, 30, 60]);
    assert.equal(
      logged.at(-1),
      "skia-source: syncing failed on try 4 of 4, the last: HTTP 429 on call 4",
    );
  });
});

// skia-bindings reads a `SKIA_SOURCE_DIR` that is set at all as an offline
// build from it, so an empty one -- what a failed `prepare` once wrote into a
// CI job's environment -- built from no directory and failed reporting gn
// missing. The script names it instead, whatever it was asked to do.
describe("skia-source refuses an empty SKIA_SOURCE_DIR", () => {
  for (const command of ["env", "dir", "prepare"]) {
    test(command, () => {
      const run = spawnSync(process.execPath, [SCRIPT, command], {
        env: { ...process.env, SKIA_SOURCE_DIR: "" },
        encoding: "utf8",
      });
      assert.equal(run.status, 1, run.stderr);
      assert.equal(run.stdout, "");
      assert.match(run.stderr, /SKIA_SOURCE_DIR is set but empty/);
    });
  }

  test("and leaves one that is unset alone", () => {
    const env = { ...process.env };
    delete env.SKIA_SOURCE_DIR;
    const run = spawnSync(process.execPath, [SCRIPT, "dir"], {
      env,
      encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
    assert.match(run.stdout, /skia-bindings[/\\]skia\n$/);
  });
});

/** A scratch directory, removed after the test that made it. */
function scratch(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "skia-source-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/**
 * Git's options that keep a checkout byte for byte what was committed: a
 * Windows runner's global `core.autocrlf` otherwise writes CRLF line endings.
 */
const NO_LINE_ENDING_CONVERSION = ["-c", "core.autocrlf=false"];

/**
 * Runs git for a test's own setup, clear of any repository the environment
 * names, so the suite behaves the same when `just ci` runs from a hook.
 */
function git(cwd, ...args) {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([name]) => !name.startsWith("GIT_")),
  );
  return execFileSync(
    "git",
    [
      "-c",
      "user.name=test",
      "-c",
      "user.email=test@example.invalid",
      "-c",
      "commit.gpgsign=false",
      "-c",
      "init.defaultBranch=main",
      ...NO_LINE_ENDING_CONVERSION,
      ...args,
    ],
    { cwd, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
}

/** A repository with one commit holding `files`, a path to its contents. */
function repository(dir, files) {
  mkdirSync(dir, { recursive: true });
  git(dir, "init", "--quiet");
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(join(dir, path, ".."), { recursive: true });
    writeFileSync(join(dir, path), contents);
  }
  git(dir, "add", "--all");
  git(dir, "commit", "--quiet", "--message", "files");
  return dir;
}

/** Every file under `dir` with its contents, to compare before and after. */
function snapshot(dir) {
  const files = {};
  const walk = (at) => {
    for (const entry of readdirSync(at, { withFileTypes: true })) {
      const path = join(at, entry.name);
      if (entry.isDirectory()) walk(path);
      else files[path] = readFileSync(path).toString("base64");
    }
  };
  walk(dir);
  return files;
}

// `git commit` runs a hook with `GIT_DIR` and `GIT_INDEX_FILE` naming the
// repository being committed to, and git honours them over the directory a
// command runs in. `just precommit` runs `prepare`, so a clone and
// git-sync-deps' `remote set-url`, `fetch` and `checkout` inherited them and
// rewrote the committing repository's index, origin and object store with
// Skia's dependencies -- and its depth-limited fetches wrote a `shallow` file
// that made the whole repository shallow.
describe("skia-source's git acts on its own checkout inside a hook", () => {
  let run;
  let saved;
  beforeEach(async () => {
    ({ run } = await import(pathToFileURL(SCRIPT).href));
    saved = { ...process.env };
  });
  afterEach(() => {
    for (const name of Object.keys(process.env))
      if (!(name in saved)) delete process.env[name];
    Object.assign(process.env, saved);
  });

  test("a clone, a remote change and a shallow fetch leave the hook's repository alone", (t) => {
    const dir = scratch(t);
    const committing = repository(join(dir, "committing"), { a: "a\n" });
    const dependency = repository(join(dir, "dependency"), { b: "b\n" });
    const checkout = join(dir, "checkout");
    const before = snapshot(join(committing, ".git"));

    process.env.GIT_DIR = join(committing, ".git");
    process.env.GIT_INDEX_FILE = join(committing, ".git", "index");
    // Each step is tried whatever the one before did, so that the comparison
    // below reports what reached the hook's repository rather than the first
    // step to fail.
    const failed = [
      [...NO_LINE_ENDING_CONVERSION, "clone", "--quiet", dependency, checkout],
      [
        "-C",
        checkout,
        "remote",
        "set-url",
        "origin",
        "https://example.invalid/dependency.git",
      ],
      // A URL, not a path: git ignores `--depth` for a local path.
      [
        "-C",
        checkout,
        "fetch",
        "--quiet",
        "--depth",
        "1",
        pathToFileURL(dependency).href,
      ],
    ].flatMap((args) => {
      try {
        run("git", args);
        return [];
      } catch (error) {
        return [error.message];
      }
    });

    assert.deepEqual(snapshot(join(committing, ".git")), before);
    assert.deepEqual(failed, []);
    // The control: the commands ran, and on the checkout.
    assert.equal(
      git(checkout, "remote", "get-url", "origin").trim(),
      "https://example.invalid/dependency.git",
    );
    assert.equal(readFileSync(join(checkout, "b"), "utf8"), "b\n");
    assert.ok(existsSync(join(checkout, ".git", "shallow")));
  });
});

/** The cache name `prepare` gives the tree for `version` and `patch`. */
function treeName(version, patch) {
  const digest = createHash("sha256").update(patch).digest("hex").slice(0, 12);
  return `skia-${version}-${digest}`;
}

/** A Cargo.lock that resolves skia-bindings to `version`. */
function lock(version) {
  return `[[package]]\nname = "skia-bindings"\nversion = "${version}"\n`;
}

const PATCH_PATH = "containers/skia-metal-untimed-compile.patch";

// `prepare` removes the trees a new one supersedes, and the cache is shared by
// every worktree: one moving to a new Skia removed the tree every other
// worktree still built from. A tree is superseded only when no worktree of the
// repository locks it.
describe("skia-source keeps the trees a worktree builds from", () => {
  let treesInUse;
  let removeSuperseded;
  beforeEach(async () => {
    ({ treesInUse, removeSuperseded } = await import(
      pathToFileURL(SCRIPT).href
    ));
    mock.method(console, "error", () => {});
  });
  afterEach(() => mock.restoreAll());

  test("each worktree names the tree its lock and patch select", (t) => {
    const dir = scratch(t);
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      [PATCH_PATH]: "first\n",
    });
    git(root, "worktree", "add", "--quiet", "-b", "next", join(dir, "next"));
    writeFileSync(join(dir, "next", "Cargo.lock"), lock("0.2.0"));
    writeFileSync(join(dir, "next", PATCH_PATH), "second\n");

    assert.deepEqual(
      treesInUse(root),
      new Set([treeName("0.1.0", "first\n"), treeName("0.2.0", "second\n")]),
    );
  });

  test("a worktree whose checkout is gone names nothing", (t) => {
    const dir = scratch(t);
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      [PATCH_PATH]: "first\n",
    });
    git(root, "worktree", "add", "--quiet", "-b", "gone", join(dir, "gone"));
    rmSync(join(dir, "gone"), { recursive: true });

    assert.deepEqual(treesInUse(root), new Set([treeName("0.1.0", "first\n")]));
  });

  test("outside a repository the trees in use are unknown", (t) => {
    assert.equal(treesInUse(scratch(t)), null);
  });

  test("only the trees no worktree uses are removed, with their partials", (t) => {
    const cache = scratch(t);
    for (const name of [
      "skia-used",
      "skia-used.partial",
      "skia-old",
      "skia-old.partial",
      "unrelated",
    ])
      mkdirSync(join(cache, name));

    removeSuperseded(cache, new Set(["skia-used"]));

    assert.deepEqual(readdirSync(cache).sort(), [
      "skia-used",
      "skia-used.partial",
      "unrelated",
    ]);
  });

  test("nothing is removed when the trees in use are unknown", (t) => {
    const cache = scratch(t);
    mkdirSync(join(cache, "skia-old"));

    removeSuperseded(cache, null);

    assert.ok(existsSync(join(cache, "skia-old")));
  });
});
