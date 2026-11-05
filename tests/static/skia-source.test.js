const assert = require("assert");
const { execFileSync, spawnSync } = require("child_process");
const { createHash } = require("crypto");
const {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");
const { pathToFileURL } = require("url");
const { describe, test, mock, beforeEach, afterEach } = require("node:test");

const SCRIPT = join(__dirname, "..", "..", "scripts", "skia-source.mjs");

/** The file `bin/fetch-gn` writes on this platform. */
const GN = process.platform === "win32" ? "gn.exe" : "gn";

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

// `env`'s line is read twice: by just, as its `dotenv-command`, and in a
// shell by `eval "export $(node scripts/skia-source.mjs env)"`. Printed bare, a
// Windows path failed the first -- just's dotenv parser took `\U` in
// `C:\Users\...` as an escape and refused the line, so every recipe failed
// before it ran -- and a path with a space failed both.
describe("skia-source env writes a line just and a shell read back whole", () => {
  let dotenvLine;
  beforeEach(async () => {
    ({ dotenvLine } = await import(pathToFileURL(SCRIPT).href));
  });

  const paths = [
    ["an ordinary path, bare", "/Users/me/.cache/x", "V=/Users/me/.cache/x"],
    [
      "a Windows path, quoted",
      "C:\\Users\\runneradmin\\.cache\\x",
      "V='C:\\Users\\runneradmin\\.cache\\x'",
    ],
    ["a path with a space, quoted", "/a b/x", "V='/a b/x'"],
    ["a quote inside, closed and escaped", "/it's/x", "V='/it'\\''s/x'"],
    ["a dollar, quoted", "/a$HOME/x", "V='/a$HOME/x'"],
  ];
  for (const [what, value, line] of paths)
    test(what, () => assert.equal(dotenvLine("V", value), line));

  /** `env`'s line for a cache under `base`, and the directory it names. */
  function envAt(base) {
    const run = spawnSync(process.execPath, [SCRIPT, "env"], {
      env: { ...process.env, XDG_CACHE_HOME: base, SKIA_SOURCE_DIR: undefined },
      encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
    const dir = spawnSync(process.execPath, [SCRIPT, "dir"], {
      env: { ...process.env, XDG_CACHE_HOME: base, SKIA_SOURCE_DIR: undefined },
      encoding: "utf8",
    }).stdout.trim();
    return { line: run.stdout.trim(), dir };
  }

  test("just reads back the directory under a cache path with a space and a quote", (t) => {
    const which = spawnSync("just", ["--version"], { encoding: "utf8" });
    if (which.status !== 0) return t.skip("just is not installed");
    const dir = scratch(t);
    const base = join(dir, "it's a cache");
    const { line, dir: expected } = envAt(base);
    assert.match(line, /^SKIA_SOURCE_DIR=/);
    writeFileSync(join(dir, "line"), `${line}\n`);
    writeFileSync(
      join(dir, "justfile"),
      "set dotenv-command := \"node -e \\\"process.stdout.write(require('fs').readFileSync('line', 'utf8'))\\\"\"\n" +
        "show:\n" +
        '    @node -e "process.stdout.write(process.env.SKIA_SOURCE_DIR)"\n',
    );
    // just loads a dotenv-command's line only for a variable not set already,
    // so one this process inherited -- every CI job that builds Skia sets
    // SKIA_SOURCE_DIR -- would be read back in place of the line.
    const env = { ...process.env };
    delete env.SKIA_SOURCE_DIR;
    const read = spawnSync("just", ["show"], {
      cwd: dir,
      env,
      encoding: "utf8",
    });
    assert.equal(read.status, 0, read.stderr);
    assert.equal(read.stdout, expected);
  });

  test("a shell reads back the directory under a cache path with a space and a quote", (t) => {
    if (process.platform === "win32")
      return t.skip("no POSIX shell to trust on Windows; just reads it there");
    const dir = scratch(t);
    const { line, dir: expected } = envAt(join(dir, "it's a cache"));
    const read = spawnSync(
      "sh",
      ["-c", 'eval "export $LINE"; printf %s "$SKIA_SOURCE_DIR"'],
      { env: { ...process.env, LINE: line }, encoding: "utf8" },
    );
    assert.equal(read.status, 0, read.stderr);
    assert.equal(read.stdout, expected);
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
 *
 * Without automatic maintenance: a commit otherwise starts `git maintenance
 * run --auto` detached, which takes `objects/maintenance.lock` in the
 * repository just committed to while the test goes on, so a snapshot of that
 * repository holds the lock or not by timing alone.
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
      "-c",
      "maintenance.auto=false",
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

describe("skia-source's git writes a repository's bytes", () => {
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

  // The fetch's clone, under the config Windows's git is installed with.
  test("a clone under core.autocrlf=true checks out LF as committed", (t) => {
    const dir = scratch(t);
    const upstream = repository(join(dir, "upstream"), { "a.cc": "a\nb\n" });
    const config = join(dir, "gitconfig");
    writeFileSync(config, "[core]\n\tautocrlf = true\n");
    process.env.GIT_CONFIG_GLOBAL = config;

    run("git", ["clone", "--quiet", upstream, join(dir, "checkout")]);
    assert.equal(readFileSync(join(dir, "checkout", "a.cc"), "utf8"), "a\nb\n");
  });

  // The other route to CRLF: a machine's own attributes marking every file
  // text, with CRLF as its line ending.
  test("a clone under core.eol=crlf and a global text attribute checks out LF", (t) => {
    const dir = scratch(t);
    const upstream = repository(join(dir, "upstream"), { "a.cc": "a\nb\n" });
    const attributes = join(dir, "gitattributes");
    writeFileSync(attributes, "* text=auto\n");
    const config = join(dir, "gitconfig");
    writeFileSync(
      config,
      `[core]\n\teol = crlf\n\tattributesFile = ${attributes.replaceAll("\\", "/")}\n`,
    );
    process.env.GIT_CONFIG_GLOBAL = config;

    run("git", ["clone", "--quiet", upstream, join(dir, "checkout")]);
    assert.equal(readFileSync(join(dir, "checkout", "a.cc"), "utf8"), "a\nb\n");
  });
});

/**
 * The cache name `prepare` gives the tree for `version` and this platform's
 * patches holding `contents`, in order.
 */
function treeName(version, contents) {
  const hash = createHash("sha256");
  for (const content of contents) hash.update(content);
  return `skia-${version}-${hash.digest("hex").slice(0, 12)}`;
}

/** A Cargo.lock that resolves skia-bindings to `version`. */
function lock(version) {
  return `[[package]]\nname = "skia-bindings"\nversion = "${version}"\n`;
}

/**
 * This platform's patch files, each holding its own name after `label`, so
 * two labels give two different trees. Returns the files and their contents.
 */
function patchFiles(patches, label) {
  const files = Object.fromEntries(
    (patches[process.platform] ?? []).map((path) => [
      path,
      `${label} ${path}\n`,
    ]),
  );
  return { files, contents: Object.values(files) };
}

// `prepare` removes the trees a new one supersedes, and the cache is shared by
// every worktree: one moving to a new Skia removed the tree every other
// worktree still built from. A tree is superseded only when no worktree of the
// repository names it, no out-dir in a worktree's target was built from it,
// and no running `just` holds it.
describe("skia-source keeps the trees a worktree builds from", () => {
  let treesInUse;
  let removeSuperseded;
  let PATCHES;
  beforeEach(async () => {
    ({ treesInUse, removeSuperseded, PATCHES } = await import(
      pathToFileURL(SCRIPT).href
    ));
    mock.method(console, "error", () => {});
  });
  afterEach(() => mock.restoreAll());

  test("each worktree names the tree its lock and patches select", (t) => {
    const dir = scratch(t);
    const first = patchFiles(PATCHES, "first");
    const second = patchFiles(PATCHES, "second");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    git(root, "worktree", "add", "--quiet", "-b", "next", join(dir, "next"));
    writeFileSync(join(dir, "next", "Cargo.lock"), lock("0.2.0"));
    for (const [path, content] of Object.entries(second.files))
      writeFileSync(join(dir, "next", path), content);

    assert.deepEqual(
      treesInUse(root),
      new Set([
        treeName("0.1.0", first.contents),
        treeName("0.2.0", second.contents),
      ]),
    );
  });

  test("a worktree whose checkout is gone names nothing", (t) => {
    const dir = scratch(t);
    const first = patchFiles(PATCHES, "first");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    git(root, "worktree", "add", "--quiet", "-b", "gone", join(dir, "gone"));
    rmSync(join(dir, "gone"), { recursive: true });

    assert.deepEqual(
      treesInUse(root),
      new Set([treeName("0.1.0", first.contents)]),
    );
  });

  // A branch adding a patch named every other worktree's tree by its own,
  // longer list, failed to read the patch they lack, and removed their trees.
  test("a worktree on another patch list keeps the tree its own script names", (t) => {
    const dir = scratch(t);
    const first = patchFiles(PATCHES, "first");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    git(root, "worktree", "add", "--quiet", "-b", "other", join(dir, "other"));
    for (const path of Object.keys(first.files))
      rmSync(join(dir, "other", path));
    mkdirSync(join(dir, "other", "scripts"), { recursive: true });
    writeFileSync(
      join(dir, "other", "scripts", "skia-source.mjs"),
      `import { join } from "node:path";
       const cache = join(process.env.XDG_CACHE_HOME, "meo-skia-canvas");
       console.log(join(cache, "skia-0.1.0-its-own", "rust-skia", "skia-bindings", "skia"));\n`,
    );
    t.after(() => {
      if (cacheHome === undefined) delete process.env.XDG_CACHE_HOME;
      else process.env.XDG_CACHE_HOME = cacheHome;
    });
    const cacheHome = process.env.XDG_CACHE_HOME;
    process.env.XDG_CACHE_HOME = join(dir, "cache");

    assert.deepEqual(
      treesInUse(root),
      new Set([treeName("0.1.0", first.contents), "skia-0.1.0-its-own"]),
    );
  });

  test("a worktree whose own script names no tree leaves the trees unknown", (t) => {
    const dir = scratch(t);
    const first = patchFiles(PATCHES, "first");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    git(
      root,
      "worktree",
      "add",
      "--quiet",
      "-b",
      "broken",
      join(dir, "broken"),
    );
    mkdirSync(join(dir, "broken", "scripts"), { recursive: true });
    writeFileSync(
      join(dir, "broken", "scripts", "skia-source.mjs"),
      "process.exit(1);\n",
    );

    assert.equal(treesInUse(root), null);
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

  // A worktree names one tree, by its patch list as it stands, but its target
  // can hold out-dirs built from others: a build that ran before the list
  // changed, or a running one. Those trees were removed from under it.
  test("a tree only a worktree's target was built from is kept", (t) => {
    const dir = scratch(t);
    const cache = withCache(t, join(dir, "cache"));
    const first = patchFiles(PATCHES, "first");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    builtFrom(root, join("aarch64-apple-darwin", "release"), [
      `cargo:rerun-if-changed=${join(cache, "skia-0.1.0-earlier", "rust-skia", "skia-bindings", "skia", "include")}`,
      // A path outside the cache whose directory happens to read as a tree.
      `cargo:rerun-if-changed=${join(dir, "worktrees", "skia-m156", "src")}`,
    ]);

    assert.deepEqual(
      treesInUse(root),
      new Set([treeName("0.1.0", first.contents), "skia-0.1.0-earlier"]),
    );
  });

  // An uncommitted edit to the patch list renames a worktree's tree at once,
  // while the build it was running, or its next one, still uses the old one.
  test("a worktree's edited patch list keeps the tree its target was built from", (t) => {
    const dir = scratch(t);
    const cache = withCache(t, join(dir, "cache"));
    const first = patchFiles(PATCHES, "first");
    const second = patchFiles(PATCHES, "second");
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
      ...first.files,
    });
    const built = treeName("0.1.0", first.contents);
    builtFrom(root, "debug", [
      `cargo:rerun-if-changed=${join(cache, built, "rust-skia", "skia-bindings", "skia", "include")}`,
    ]);
    for (const [path, content] of Object.entries(second.files))
      writeFileSync(join(root, path), content);

    const inUse = treesInUse(root);
    assert.ok(inUse.has(treeName("0.1.0", second.contents)));
    assert.ok(inUse.has(built), `${built} was dropped for the edited list`);
  });

  // `env` is the justfile's `dotenv-command`, so it runs as a child of the
  // `just` that then runs the recipe, and holds the tree for as long as that
  // `just` lives.
  test("a tree a running build holds is kept, and released when it ends", async (t) => {
    const dir = scratch(t);
    const cache = withCache(t, join(dir, "cache"));
    const name = treeNameOf(cache, PATCHES);
    if (name === null) return; // No patches here, so `env` names no tree.
    const root = repository(join(dir, "root"), {
      "Cargo.lock": lock("0.1.0"),
    });
    const { treesToKeep } = await import(pathToFileURL(SCRIPT).href);

    const holder = await holdingParent(cache);
    try {
      assert.ok(treesToKeep(root, cache).has(name), `${name} was not held`);
    } finally {
      await new Promise((done) => {
        holder.once("exit", done);
        holder.kill();
      });
    }
    assert.ok(!treesToKeep(root, cache).has(name), `${name} is still held`);
  });

  // A holder's PID can be given to another process once it ends; that process
  // holds nothing.
  test("a hold whose PID now runs another process holds nothing", async (t) => {
    if (process.platform === "win32") return; // No start time to compare.
    const cache = withCache(t, join(scratch(t), "cache"));
    const { hold, treesHeld } = await import(pathToFileURL(SCRIPT).href);

    hold("skia-0.1.0-held", process.pid, cache);
    assert.deepEqual(treesHeld(cache), new Set(["skia-0.1.0-held"]));
    writeFileSync(
      join(cache, "holds", "skia-0.1.0-held", String(process.pid)),
      "Thu Jan  1 00:00:00 1970\n",
    );
    assert.deepEqual(treesHeld(cache), new Set());
    assert.ok(!existsSync(join(cache, "holds", "skia-0.1.0-held")));
  });
});

/** Points `XDG_CACHE_HOME` at `dir` for the test; returns the cache inside. */
function withCache(t, dir) {
  const before = process.env.XDG_CACHE_HOME;
  t.after(() => {
    if (before === undefined) delete process.env.XDG_CACHE_HOME;
    else process.env.XDG_CACHE_HOME = before;
  });
  process.env.XDG_CACHE_HOME = dir;
  const cache = join(dir, "meo-skia-canvas");
  mkdirSync(cache, { recursive: true });
  return cache;
}

/**
 * Writes a skia-bindings out-dir's `output` holding `lines` into the
 * `profile` directory of `worktree`'s target -- `debug`, or a target triple
 * and a profile.
 */
function builtFrom(worktree, profile, lines) {
  const dir = join(
    worktree,
    "target",
    profile,
    "build",
    "skia-bindings-0123456789abcdef",
  );
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    join(dir, "output"),
    `cargo:rustc-link-lib=static=skia\n${lines.join("\n")}\n`,
  );
}

/**
 * The tree this checkout's `env` names under `cache`, or null on a platform
 * with no patches, where it names none. Asked of `dir`, which holds nothing.
 */
function treeNameOf(cache, patches) {
  if ((patches[process.platform] ?? []).length === 0) return null;
  const dir = execFileSync(process.execPath, [SCRIPT, "dir"], {
    encoding: "utf8",
    env: { ...process.env, XDG_CACHE_HOME: join(cache, "..") },
  }).trim();
  return dir.slice(cache.length + 1).split(/[\\/]/)[0];
}

/**
 * A process that runs this checkout's `env`, as `just` runs its
 * `dotenv-command`, and then stays alive until it is killed.
 */
function holdingParent(cache) {
  const { spawn } = require("child_process");
  const holder = spawn(
    process.execPath,
    [
      "-e",
      `require("child_process").execFileSync(process.execPath, [${JSON.stringify(SCRIPT)}, "env"], { stdio: ["ignore", "ignore", "inherit"] });
       console.log("held");
       setInterval(() => {}, 1000);`,
    ],
    {
      env: { ...process.env, XDG_CACHE_HOME: join(cache, "..") },
      stdio: ["ignore", "pipe", "inherit"],
    },
  );
  return new Promise((resolve, reject) => {
    holder.once("exit", (code) =>
      reject(new Error(`the holder exited with ${code}`)),
    );
    holder.stdout.once("data", () => {
      holder.removeAllListeners("exit");
      resolve(holder);
    });
  });
}

// `just use-local-skia` points skia-bindings at a rust-skia checkout, which
// skia-bindings then builds itself, DEPS and all. A tree named or prepared
// here would be built offline in its place -- unsynced, in the release
// containers that use a checkout.
describe("skia-source stands aside for a local rust-skia checkout", () => {
  let usesLocalSkia;
  beforeEach(async () => {
    ({ usesLocalSkia } = await import(pathToFileURL(SCRIPT).href));
  });

  /** The skia-bindings line `use-local-skia` appends, read from the justfile. */
  const localLine = () => {
    const justfile = readFileSync(
      join(__dirname, "..", "..", "justfile"),
      "utf8",
    );
    const found = justfile.match(/echo '(skia-bindings = \{ path = [^']+)'/);
    assert.ok(found, "use-local-skia still writes a skia-bindings path");
    return found[1];
  };

  test("the line use-local-skia writes is recognised", (t) => {
    const root = scratch(t);
    writeFileSync(
      join(root, "Cargo.toml"),
      `[dependencies]\nskia-safe = "0.156.0"\n\n[patch.crates-io]\n${localLine()}\n`,
    );
    assert.equal(usesLocalSkia(root), true);
  });

  test("a version from crates.io is not", (t) => {
    const root = scratch(t);
    writeFileSync(
      join(root, "Cargo.toml"),
      '[dependencies]\nskia-safe = "0.156.0"\nskia-bindings = "0.156.0"\n',
    );
    assert.equal(usesLocalSkia(root), false);
  });
});

/**
 * A prepared tree for `version` under `cache`, named `name`, built the way
 * `prepare` builds one but from local repositories: rust-skia at the
 * version's tag with Skia as its submodule, one dependency DEPS pins checked
 * out at its pin, and a gn. `src/a.cc` is the file a patch changes; `patched`
 * leaves it changed, as a tree with a different patch list would be.
 */
function preparedTree(dir, cache, name, version, { patched = true } = {}) {
  const sources = join(dir, `sources-${name}`);
  const dependency = repository(join(sources, "dep"), { "dep.c": "dep\n" });
  const pin = git(dependency, "rev-parse", "HEAD").trim();
  const skia = repository(join(sources, "skia"), {
    // A URL, as a real DEPS names its repositories: a Windows path in a
    // Python string reads `\U` as the start of an escape.
    DEPS: `deps = {"third_party/externals/dep": "${pathToFileURL(dependency).href}@${pin}"}\n`,
    "src/a.cc": "pristine\n",
  });
  const rustSkia = repository(join(sources, "rust-skia"), { README: "r\n" });
  const local = ["-c", "protocol.file.allow=always"];
  git(
    rustSkia,
    ...local,
    "submodule",
    "add",
    "--quiet",
    skia,
    "skia-bindings/skia",
  );
  git(rustSkia, "commit", "--quiet", "--message", "skia");
  git(rustSkia, "tag", version);

  const tree = join(cache, name);
  const checkout = join(tree, "rust-skia");
  git(
    dir,
    ...local,
    "clone",
    "--quiet",
    "--recurse-submodules",
    "--branch",
    version,
    rustSkia,
    checkout,
  );
  const source = join(checkout, "skia-bindings", "skia");
  git(
    dir,
    "clone",
    "--quiet",
    dependency,
    join(source, "third_party", "externals", "dep"),
  );
  mkdirSync(join(source, "bin"), { recursive: true });
  writeFileSync(join(source, "bin", GN), "gn\n");
  if (patched) writeFileSync(join(source, "src", "a.cc"), "patched\n");
  writeFileSync(join(tree, ".ready"), `${version}\n`);
  return {
    tree,
    checkout,
    source,
    dependency: join(source, "third_party", "externals", "dep"),
  };
}

// A changed patch list names a new tree, and `prepare` fetched every new tree
// from the network -- rust-skia, Skia's DEPS and gn, about 1 GB -- though every
// tree for a version holds the same checkout and dependencies and differs only
// in the patches applied to tracked files.
describe("skia-source derives a tree from another of its version", () => {
  let derive, patchCreates, copyTree, baseRefusal;
  beforeEach(async () => {
    ({ derive, patchCreates, copyTree, baseRefusal } = await import(
      pathToFileURL(SCRIPT).href
    ));
  });

  test("a patched tree is copied with its sources restored and nothing fetched", (t) => {
    const dir = scratch(t);
    const cache = join(dir, "cache");
    const base = preparedTree(dir, cache, "skia-1.0.0-aaaa", "1.0.0");
    const partial = join(cache, "skia-1.0.0-bbbb.partial");
    t.mock.method(console, "error", () => {});

    assert.equal(derive(cache, partial, "1.0.0"), "skia-1.0.0-aaaa");
    const source = join(partial, "rust-skia", "skia-bindings", "skia");
    assert.equal(
      readFileSync(join(source, "src", "a.cc"), "utf8"),
      "pristine\n",
    );
    assert.equal(readFileSync(join(source, "bin", GN), "utf8"), "gn\n");
    assert.equal(
      readFileSync(
        join(source, "third_party", "externals", "dep", "dep.c"),
        "utf8",
      ),
      "dep\n",
    );
    assert.ok(
      !existsSync(join(partial, ".ready")),
      "the caller marks it ready",
    );
    // The base is left as it was.
    assert.equal(
      readFileSync(join(base.source, "src", "a.cc"), "utf8"),
      "patched\n",
    );
  });

  // Windows's git is installed with core.autocrlf true, so a restore that
  // follows the machine's config wrote Skia's sources with CRLF: not the bytes
  // a tree is named for, and an LF patch then fails to apply at `-F0`.
  test("a restore under core.autocrlf=true writes the repository's bytes", (t) => {
    const dir = scratch(t);
    const cache = join(dir, "cache");
    preparedTree(dir, cache, "skia-1.0.0-aaaa", "1.0.0");
    const partial = join(cache, "skia-1.0.0-bbbb.partial");
    t.mock.method(console, "error", () => {});
    const config = join(dir, "gitconfig");
    writeFileSync(config, "[core]\n\tautocrlf = true\n");
    const before = process.env.GIT_CONFIG_GLOBAL;
    t.after(() => {
      if (before === undefined) delete process.env.GIT_CONFIG_GLOBAL;
      else process.env.GIT_CONFIG_GLOBAL = before;
    });
    process.env.GIT_CONFIG_GLOBAL = config;

    assert.equal(derive(cache, partial, "1.0.0"), "skia-1.0.0-aaaa");
    const source = join(partial, "rust-skia", "skia-bindings", "skia");
    assert.equal(
      readFileSync(join(source, "src", "a.cc"), "utf8"),
      "pristine\n",
    );
  });

  // Each way a base can fail to be a whole preparation, and what `derive`
  // must say about it. None may leave a partial tree behind.
  const defects = [
    [
      "its dependency off its pin",
      ({ dependency }) => {
        writeFileSync(join(dependency, "dep.c"), "moved\n");
        git(dependency, "commit", "--quiet", "--all", "--message", "moved");
      },
      /is not at the commit DEPS pins/,
    ],
    [
      "its dependency edited",
      ({ dependency }) => {
        writeFileSync(join(dependency, "dep.c"), "edited\n");
      },
      /has been edited/,
    ],
    ["no gn", ({ source }) => rmSync(join(source, "bin", GN))],
    [
      "a DEPS Python cannot read",
      ({ source }) => {
        writeFileSync(join(source, "DEPS"), 'deps = {"x": "C:\\Users\\x@0"}\n');
      },
      /its DEPS could not be read: .*SyntaxError/,
    ],
    [
      "a ready mark for another version",
      ({ tree }) => {
        writeFileSync(join(tree, ".ready"), "0.9.0\n");
      },
      /not a finished tree/,
    ],
    [
      "no ready mark",
      ({ tree }) => rmSync(join(tree, ".ready")),
      /not a finished tree/,
    ],
    [
      "its checkout moved off the tag",
      ({ checkout }) => {
        writeFileSync(join(checkout, "README"), "moved\n");
        git(checkout, "commit", "--quiet", "--all", "--message", "moved");
      },
      /not at the 1.0.0 tag/,
    ],
    [
      "Skia moved off the commit rust-skia records",
      ({ source }) => {
        git(source, "commit", "--quiet", "--allow-empty", "--message", "moved");
      },
      /not at the commit rust-skia records/,
    ],
  ].map(([what, damage, reason = /no gn/]) => ({ what, damage, reason }));

  for (const { what, damage, reason } of defects)
    test(`a base with ${what} is refused`, (t) => {
      const dir = scratch(t);
      const cache = join(dir, "cache");
      damage(preparedTree(dir, cache, "skia-1.0.0-aaaa", "1.0.0"));
      const partial = join(cache, "skia-1.0.0-bbbb.partial");
      const said = [];
      t.mock.method(console, "error", (line) => said.push(line));

      assert.equal(derive(cache, partial, "1.0.0"), null);
      assert.ok(!existsSync(partial), "nothing is left behind");
      assert.match(said.join("\n"), reason);
    });

  test("a base that holds a file a patch creates is refused", (t) => {
    const dir = scratch(t);
    const cache = join(dir, "cache");
    const { checkout } = preparedTree(dir, cache, "skia-1.0.0-aaaa", "1.0.0");
    writeFileSync(join(checkout, "NEW"), "left over\n");
    const said = [];
    t.mock.method(console, "error", (line) => said.push(line));

    const partial = join(cache, "skia-1.0.0-bbbb.partial");
    assert.equal(derive(cache, partial, "1.0.0", ["NEW"]), null);
    assert.match(said.join("\n"), /NEW exists, which a patch creates/);
  });

  // `bin/fetch-gn` writes `gn.exe` on Windows. Looked for as `bin/gn` there,
  // every Windows tree read as unfinished, and none was ever derived from.
  test("gn is looked for under the name fetch-gn gives it on each platform", (t) => {
    const dir = scratch(t);
    const { tree, source } = preparedTree(
      dir,
      join(dir, "cache"),
      "skia-1.0.0-aaaa",
      "1.0.0",
    );
    const bin = join(source, "bin");
    const only = (name) => {
      for (const gn of ["gn", "gn.exe"]) rmSync(join(bin, gn), { force: true });
      writeFileSync(join(bin, name), "gn\n");
    };
    only("gn.exe");
    assert.equal(baseRefusal(tree, "1.0.0", [], "win32"), null);
    assert.equal(baseRefusal(tree, "1.0.0", [], "darwin"), "it has no gn");
    only("gn");
    assert.equal(baseRefusal(tree, "1.0.0", [], "win32"), "it has no gn");
    assert.equal(baseRefusal(tree, "1.0.0", [], "linux"), null);
  });

  test("a tree for another version is not considered", (t) => {
    const dir = scratch(t);
    const cache = join(dir, "cache");
    preparedTree(dir, cache, "skia-0.9.0-aaaa", "0.9.0");
    const said = [];
    t.mock.method(console, "error", (line) => said.push(line));

    assert.equal(
      derive(cache, join(cache, "skia-1.0.0-bbbb.partial"), "1.0.0"),
      null,
    );
    assert.deepEqual(said, []);
  });

  // Windows has no `cp`, so the copy there is Node's. Run here too, where it
  // can be: what it makes has to be the tree, links included.
  test("Windows's copy makes the tree, its links as links", (t) => {
    const dir = scratch(t);
    const from = join(dir, "from");
    mkdirSync(join(from, "a", "b"), { recursive: true });
    writeFileSync(join(from, "a", "b", "file"), "contents\n");
    // Windows lets an account without the right make no link: the copy is
    // still checked there, its link only where one could be made.
    let linked = true;
    try {
      symlinkSync("..", join(from, "a", "up"), "dir");
    } catch (error) {
      if (error.code !== "EPERM") throw error;
      linked = false;
    }
    copyTree(from, join(dir, "to"), "win32");
    assert.equal(
      readFileSync(join(dir, "to", "a", "b", "file"), "utf8"),
      "contents\n",
    );
    if (linked) assert.equal(readlinkSync(join(dir, "to", "a", "up")), "..");
  });

  test("the files a patch creates are the ones it takes from /dev/null", () => {
    const patch = [
      "--- a/skia-bindings/skia/src/a.cc",
      "+++ b/skia-bindings/skia/src/a.cc",
      "@@ -1 +1 @@",
      "-pristine",
      "+patched",
      "--- /dev/null",
      "+++ b/skia-bindings/skia/src/new.cc",
      "@@ -0,0 +1 @@",
      "+new",
      "",
    ].join("\n");
    assert.deepEqual(patchCreates(patch), ["skia-bindings/skia/src/new.cc"]);
    assert.deepEqual(patchCreates(patch.replace("/dev/null", "a/x")), []);
  });
});

// `cargo-patch` points a build at the prepared checkout's rust-skia crates,
// which carry the Graphite bindings, by a `.cargo/config.toml` of its own:
// written only where asked, never over a developer's own config, and removed
// only where it wrote one.
describe("skia-source cargo-patch writes only its own cargo config", () => {
  let cargoPatch;
  let cargoPatchConfig;
  let CARGO_PATCH_HEADER;
  let root;
  let tree;
  beforeEach(async () => {
    ({ cargoPatch, cargoPatchConfig, CARGO_PATCH_HEADER } = await import(
      pathToFileURL(SCRIPT).href
    ));
    root = mkdtempSync(join(tmpdir(), "cargo-patch-root-"));
    tree = mkdtempSync(join(tmpdir(), "cargo-patch-tree-"));
  });
  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
    rmSync(tree, { recursive: true, force: true });
  });

  test("the config patches the three crates from the checkout, and nothing else", () => {
    const config = cargoPatchConfig(tree);
    assert.strictEqual(config.split("\n")[0], CARGO_PATCH_HEADER);
    assert.match(config, /^\[patch\.crates-io\]$/m);
    for (const crate of ["skia-bindings", "skia-safe", "skia-svg-macros"]) {
      const path = JSON.stringify(join(tree, "rust-skia", crate));
      assert.ok(config.includes(`${crate} = { path = ${path} }`), crate);
    }
    assert.strictEqual(config.match(/ = \{ path = /g).length, 3);
  });

  test("an unprepared tree is refused, and nothing is written", () => {
    assert.throws(() => cargoPatch(root, tree), /is not prepared/);
    assert.ok(!existsSync(join(root, ".cargo", "config.toml")));
  });

  test("a prepared tree is written, and removed again", () => {
    writeFileSync(join(tree, ".ready"), "");
    const path = cargoPatch(root, tree);
    assert.strictEqual(readFileSync(path, "utf8"), cargoPatchConfig(tree));
    assert.strictEqual(cargoPatch(root, tree, { remove: true }), null);
    assert.ok(!existsSync(path));
  });

  test("a developer's own config is neither replaced nor removed", () => {
    writeFileSync(join(tree, ".ready"), "");
    mkdirSync(join(root, ".cargo"));
    const own = join(root, ".cargo", "config.toml");
    writeFileSync(own, "[build]\njobs = 4\n");
    assert.throws(
      () => cargoPatch(root, tree),
      /was not written by cargo-patch/,
    );
    assert.throws(
      () => cargoPatch(root, tree, { remove: true }),
      /was not written by cargo-patch/,
    );
    assert.strictEqual(readFileSync(own, "utf8"), "[build]\njobs = 4\n");
  });
});

// The Linux release build patches its own rust-skia checkout with `apply`, so
// it and `prepare` share one loop, one order and one refusal.
describe("skia-source applies a patch list in its order, refusing fuzz", () => {
  let applyPatches;
  beforeEach(async () => {
    ({ applyPatches } = await import(pathToFileURL(SCRIPT).href));
  });

  /** A root holding `containers/<name>.patch` for each of `patches`. */
  function root(t, patches) {
    const dir = scratch(t);
    mkdirSync(join(dir, "containers"));
    for (const [name, text] of Object.entries(patches))
      writeFileSync(join(dir, "containers", `${name}.patch`), text);
    return dir;
  }
  const edit = (from, to, context = ["x", "y"]) =>
    [
      "--- a/f.txt",
      "+++ b/f.txt",
      "@@ -1,3 +1,3 @@",
      ` ${context[0]}`,
      `-${from}`,
      `+${to}`,
      ` ${context[1]}`,
      "",
    ].join("\n");
  const checkout = (t, text) => {
    const dir = scratch(t);
    writeFileSync(join(dir, "f.txt"), text);
    return dir;
  };

  test("each patch applies after the one before it", (t) => {
    const r = root(t, { one: edit("a", "b"), two: edit("b", "c") });
    const c = checkout(t, "x\na\ny\n");
    applyPatches(c, ["containers/one.patch", "containers/two.patch"], r);
    assert.equal(readFileSync(join(c, "f.txt"), "utf8"), "x\nc\ny\n");
  });

  test("out of order, the patch that fails is named", (t) => {
    const r = root(t, { one: edit("a", "b"), two: edit("b", "c") });
    const c = checkout(t, "x\na\ny\n");
    assert.throws(
      () =>
        applyPatches(c, ["containers/two.patch", "containers/one.patch"], r),
      /containers[/\\]two\.patch does not apply/,
    );
  });

  test("a hunk whose context has moved is refused, not placed by fuzz", (t) => {
    const r = root(t, { one: edit("a", "b", ["moved", "y"]) });
    const c = checkout(t, "x\na\ny\n");
    assert.throws(
      () => applyPatches(c, ["containers/one.patch"], r),
      /containers[/\\]one\.patch does not apply/,
    );
    assert.equal(readFileSync(join(c, "f.txt"), "utf8"), "x\na\ny\n");
  });

  test("a patch already applied is refused, not undone", (t) => {
    const r = root(t, { one: edit("a", "b") });
    const c = checkout(t, "x\na\ny\n");
    applyPatches(c, ["containers/one.patch"], r);
    assert.throws(
      () => applyPatches(c, ["containers/one.patch"], r),
      /containers[/\\]one\.patch does not apply/,
    );
    assert.equal(readFileSync(join(c, "f.txt"), "utf8"), "x\nb\ny\n");
  });

  test("the command names its usage without a checkout", () => {
    const run = spawnSync(process.execPath, [SCRIPT, "apply"], {
      encoding: "utf8",
    });
    assert.equal(run.status, 2);
    assert.match(run.stderr, /usage: .* apply <rust-skia checkout>/);
  });

  test("the command fails on a checkout this platform's first patch misses", async (t) => {
    const { PATCHES } = await import(pathToFileURL(SCRIPT).href);
    const list = PATCHES[process.platform];
    if (!list) return t.skip(`no patches for ${process.platform}`);
    const run = spawnSync(process.execPath, [SCRIPT, "apply", scratch(t)], {
      encoding: "utf8",
    });
    assert.equal(run.status, 1, run.stderr);
    assert.equal(run.stdout, "");
    assert.ok(run.stderr.includes(`${list[0]} does not apply`), run.stderr);
  });
});

// CI caches a tree under `cache-key`, so a key follows `PATCHES` and the lock
// by construction rather than by a list of patch files kept beside them.
describe("skia-source names a tree's cache key", () => {
  test("alone on stdout, as the tree's own name for this platform", () => {
    const key = spawnSync(process.execPath, [SCRIPT, "cache-key"], {
      encoding: "utf8",
    });
    assert.equal(key.status, 0, key.stderr);
    const dir = spawnSync(process.execPath, [SCRIPT, "dir"], {
      encoding: "utf8",
    }).stdout.trim();
    const tree = dir.split(/[/\\]/).at(-4);
    assert.equal(key.stdout, `skia-source-${process.platform}-${tree}\n`);
  });

  test("editing a patch, or the locked version, moves the key", async (t) => {
    const { cacheKey, PATCHES } = await import(pathToFileURL(SCRIPT).href);
    const list = PATCHES[process.platform];
    if (!list) return t.skip(`no patches for ${process.platform}`);
    const repo = join(__dirname, "..", "..");
    const r = scratch(t);
    mkdirSync(join(r, "containers"));
    writeFileSync(
      join(r, "Cargo.lock"),
      readFileSync(join(repo, "Cargo.lock")),
    );
    for (const patch of list)
      writeFileSync(join(r, patch), readFileSync(join(repo, patch)));
    const before = cacheKey(r);
    assert.equal(before, cacheKey(repo));

    writeFileSync(
      join(r, list.at(-1)),
      `${readFileSync(join(r, list.at(-1)))}\n`,
    );
    const edited = cacheKey(r);
    assert.notEqual(edited, before);

    const lock = readFileSync(join(r, "Cargo.lock"), "utf8");
    writeFileSync(
      join(r, "Cargo.lock"),
      lock.replace(/(name = "skia-bindings"\nversion = ")[^"]+/, "$19.9.9"),
    );
    assert.notEqual(cacheKey(r), edited);
  });
});
