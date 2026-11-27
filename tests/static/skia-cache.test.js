// @ts-check

"use strict";

// scripts/skia-cache.mjs runs cargo for every recipe that builds Skia. Run
// with no `SKIA_SOURCE_DIR` on a platform with patches, skia-bindings builds
// against rust-skia's unpatched Skia, and every patch test then expects stock
// Skia and passes. So a build through the script refuses there, naming the
// command that sets the variable, unless `SKIA_CACHE_UNPATCHED=1` asks for
// unpatched Skia on purpose.

const assert = require("assert");
const { spawnSync } = require("child_process");
const { join, sep } = require("path");
const { pathToFileURL } = require("url");
const { describe, test } = require("node:test");

const SCRIPT = join(__dirname, "..", "..", "scripts", "skia-cache.mjs");
const SOURCE = join(__dirname, "..", "..", "scripts", "skia-source.mjs");

/** The command the refusal has to name, as `skia-source.mjs env` is used. */
const SETS_IT = `eval "$(node scripts/skia-source.mjs env | sed 's/^/export /')"`;

/**
 * Runs the script with `args` and `vars` over an environment that names no
 * Skia tree and links no cached Skia.
 * @param {string[]} args
 * @param {Record<string, string>} [vars]
 */
function run(args, vars = {}) {
  const env = { ...process.env };
  for (const name of [
    "SKIA_SOURCE_DIR",
    "SKIA_CACHE_UNPATCHED",
    "SKIA_LIBRARY_SEARCH_PATH",
    "SKIA_BUILD_DEFINES",
  ])
    delete env[name];
  return spawnSync(process.execPath, [SCRIPT, ...args], {
    env: { ...env, ...vars },
    encoding: "utf8",
  });
}

describe("skia-cache refuses to build unpatched Skia unasked", async () => {
  // The rule `skia-source.mjs env` applies, so every case below is read the
  // same way on a platform with no patches, where nothing names a tree.
  const { namesSourceTree } = await import(pathToFileURL(SOURCE).href);
  const named = namesSourceTree();

  test("cargo with no SKIA_SOURCE_DIR refuses, naming the fix", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    const result = run(["cargo", "--version"]);
    assert.notStrictEqual(result.status, 0, result.stdout);
    assert.doesNotMatch(result.stdout, /^cargo \d/m, "cargo ran");
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
    assert.ok(result.stderr.includes("SKIA_CACHE_UNPATCHED=1"), result.stderr);
  });

  test("fill with no SKIA_SOURCE_DIR refuses before it builds", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    // A cross target, which fill refuses for a reason of its own: the
    // refusal has to come first, and name the variable.
    const result = run(["fill", "--target", "wasm32-unknown-unknown"]);
    assert.notStrictEqual(result.status, 0);
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
  });

  test("entry with no SKIA_SOURCE_DIR refuses rather than printing none", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    const result = run(["entry"]);
    assert.notStrictEqual(result.status, 0, result.stdout);
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
  });

  test("SKIA_CACHE_UNPATCHED=1 builds unpatched Skia, and says so", () => {
    const result = run(["cargo", "--version"], { SKIA_CACHE_UNPATCHED: "1" });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.match(result.stdout, /^cargo \d/m);
    if (named) assert.match(result.stderr, /unpatched/);
  });

  test("a named tree runs cargo", () => {
    const result = run(["cargo", "--version"], { SKIA_SOURCE_DIR: "/tree" });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.match(result.stdout, /^cargo \d/m);
  });
});

describe("the refusal, case by case", async () => {
  const { unpatchedRefusal } = await import(pathToFileURL(SCRIPT).href);
  /** @type {[Record<string, string>, boolean, RegExp | null, string][]} */
  const cases = [
    [{}, true, /SKIA_SOURCE_DIR is not set/, "no tree named"],
    [{ SKIA_SOURCE_DIR: "" }, true, /SKIA_SOURCE_DIR is empty/, "an empty one"],
    [{ SKIA_SOURCE_DIR: "/tree" }, true, null, "a tree named"],
    [{}, false, null, "a platform or checkout that names no tree"],
    [{ SKIA_CACHE_UNPATCHED: "1" }, true, null, "unpatched asked for"],
    [
      { SKIA_CACHE_UNPATCHED: "yes" },
      true,
      /SKIA_CACHE_UNPATCHED is "yes"/,
      "a value other than 1",
    ],
    [
      { SKIA_CACHE_UNPATCHED: "1", SKIA_SOURCE_DIR: "/tree" },
      true,
      /SKIA_CACHE_UNPATCHED=1 asks for unpatched Skia, but SKIA_SOURCE_DIR names/,
      "unpatched asked for beside a named tree",
    ],
  ];
  for (const [env, namesTree, refused, label] of cases)
    test(label, () => {
      const why = unpatchedRefusal(env, namesTree);
      if (refused === null) assert.strictEqual(why, null);
      else assert.match(why ?? "", refused);
    });
});

// An entry's key names the macOS SDK Skia was compiled against. One SDK has
// several paths -- `MacOSX.sdk`, `MacOSX27.2.sdk` and `MacOSX27.sdk` are
// symlinks to one another -- and `MacOSX.sdk` names whichever SDK the
// installed Xcode carries, so the key is the version the SDK itself records,
// read from the directory a path resolves to.
describe("an entry's SDK is named by its version", async () => {
  const { sdkKey, currentSdk, loggedSdk, entryDir, keyFor, store, linkable } =
    await import(
      pathToFileURL(join(__dirname, "..", "..", "scripts", "skia-binaries.mjs"))
        .href
    );
  const { treeName } = await import(pathToFileURL(SOURCE).href);
  const {
    mkdirSync,
    mkdtempSync,
    realpathSync,
    rmSync,
    symlinkSync,
    writeFileSync,
  } = require("fs");
  const { tmpdir } = require("os");

  const root = realpathSync(mkdtempSync(join(tmpdir(), "skia-cache-sdk-")));
  process.on("exit", () => rmSync(root, { recursive: true, force: true }));

  /**
   * An SDK directory at `path` holding only `SDKSettings.json`, with
   * `settings` as its contents.
   * @param {string} path
   * @param {unknown} settings
   */
  function sdk(path, settings) {
    const dir = join(root, path);
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      join(dir, "SDKSettings.json"),
      typeof settings === "string" ? settings : JSON.stringify(settings),
    );
    return dir;
  }

  const xcode = sdk("Xcode/SDKs/MacOSX.sdk", { Version: "27.2" });
  /** A compiler, as `compilerKey` names one. */
  const clang = { cc: "clang version 1", cxx: "clang version 1" };
  const versioned = join(root, "Xcode/SDKs/MacOSX27.2.sdk");
  symlinkSync(xcode, versioned);
  const tools = sdk("CommandLineTools/SDKs/MacOSX27.sdk", { Version: "27.2" });
  const older = sdk("CommandLineTools/SDKs/MacOSX26.sdk", { Version: "26.5" });
  const updated = sdk("XcodeNext/SDKs/MacOSX.sdk", { Version: "28.0" });

  test("a directory and a symlink to it give the same key", () => {
    assert.deepStrictEqual(sdkKey(versioned), sdkKey(xcode));
    assert.deepStrictEqual(sdkKey(xcode), { sdk: "MacOSX27.2.sdk" });
  });

  test("two directories of one version under other names give one key", () => {
    assert.deepStrictEqual(sdkKey(tools), sdkKey(xcode));
  });

  test("the version is read, not parsed from the name", () => {
    assert.deepStrictEqual(sdkKey(older), { sdk: "MacOSX26.5.sdk" });
  });

  test("another version under the same name gives another key", () => {
    assert.notDeepStrictEqual(sdkKey(updated), sdkKey(xcode));
  });

  /** @type {[string, string][]} */
  const unreadable = [
    [join(root, "missing/MacOSX.sdk"), "no directory there"],
    [sdk("bare/MacOSX.sdk", {}), "no Version"],
    [sdk("garbled/MacOSX.sdk", "{"), "SDKSettings.json is not JSON"],
    [sdk("number/MacOSX.sdk", { Version: 27.2 }), "a Version that is a number"],
    [
      sdk("escape/MacOSX.sdk", { Version: "../27" }),
      "a Version that is a path",
    ],
  ];
  for (const [path, label] of unreadable)
    test(`an SDK with no readable version gives no key: ${label}`, () => {
      const key = sdkKey(path);
      assert.strictEqual(key.sdk, undefined, JSON.stringify(key));
      assert.ok(key.why?.includes(path), key.why);
    });

  test("a store and a lookup of one SDK spelled two ways find one entry", () => {
    const log = `cargo:warning=clang --sysroot=${versioned} -c a.cpp`;
    const stored = loggedSdk(log, "darwin");
    const looked = currentSdk({ SDKROOT: xcode }, "darwin");
    assert.deepStrictEqual(stored, { sdk: "MacOSX27.2.sdk" });
    const key = {
      tree: "t",
      target: "aarch64-apple-darwin",
      compiler: clang,
      profile: "dev",
    };
    assert.strictEqual(
      entryDir({ ...key, sdk: stored.sdk, features: [] }, root),
      entryDir({ ...key, sdk: looked.sdk, features: [] }, root),
    );
  });

  test("a store and a lookup refuse an SDK with no readable version", () => {
    const bare = join(root, "bare/MacOSX.sdk");
    assert.match(
      loggedSdk(`--sysroot=${bare}`, "darwin").why ?? "",
      /no version/,
    );
    assert.match(
      currentSdk({ SDKROOT: bare }, "darwin").why ?? "",
      /no version/,
    );
  });

  // `keyFor` and `store` read the platform they run on, and off macOS reach
  // cargo before the SDK matters.
  const onMac = process.platform === "darwin";

  test("a lookup with an unreadable SDK finds no entry, and says why", (t) => {
    if (!onMac) return t.skip("the SDK is part of the key on macOS only");
    const bare = join(root, "bare/MacOSX.sdk");
    const { key, why } = keyFor(["build"], {
      SKIA_SOURCE_DIR: "/tree",
      SDKROOT: bare,
    });
    assert.strictEqual(key, undefined);
    assert.match(why ?? "", /records no version/);
  });

  test("a build compiled against an unreadable SDK is not stored", (t) => {
    if (!onMac) return t.skip("the SDK is part of the key on macOS only");
    const cache = join(root, "cache");
    const build = join(root, "build");
    mkdirSync(build, { recursive: true });
    const sysroot = join(root, "bare/MacOSX.sdk");
    writeFileSync(
      join(build, "output"),
      `${join(cache, treeName())}/ninja\nclang --sysroot=${sysroot} -c a.cpp\n`,
    );
    assert.throws(
      () =>
        store(
          build,
          { profile: "dev", features: [], compiler: clang, since: 0 },
          () => null,
          { cache },
        ),
      /is not stored: the SDK at .* records no version/,
    );
  });

  // A build that links an entry has no ninja record to name what it links,
  // so the entry keeps the record of the build that stored it.
  describe("an entry keeps what its build compiled, for the notices", () => {
    const { existsSync, readFileSync, unlinkSync } = require("fs");
    const cache = join(root, "notices-cache");
    const build = join(root, "notices-build");
    const skia = join(build, "out", "skia");
    mkdirSync(skia, { recursive: true });
    writeFileSync(
      join(build, "output"),
      `${join(cache, treeName())}/ninja\nclang --sysroot=${xcode} -c a.cpp\n`,
    );
    writeFileSync(join(skia, "libskia.a"), "skia");
    writeFileSync(join(skia, "skia-defines.txt"), "SK_X\n");
    const compiled = { tree: "/tree", projects: ["skia", "zlib"] };
    const notices = () => compiled;
    const stored = () =>
      store(
        build,
        { profile: "dev", features: [], compiler: clang, since: 0 },
        () => null,
        { cache, notices },
      );

    test("a new entry carries the record, and is linked", () => {
      const { dir, stored: wrote } = stored();
      assert.ok(wrote);
      assert.deepStrictEqual(
        JSON.parse(readFileSync(join(dir, "notices.json"), "utf8")),
        compiled,
      );
      assert.strictEqual(
        linkable(dir, "/tree", clang, () => null),
        null,
      );
    });

    test("an entry without the record has it derived from its seed log, and is linked", () => {
      const { dir } = stored();
      unlinkSync(join(dir, "notices.json"));
      const derived = { tree: "/tree", projects: ["skia", "expat"] };
      /** @type {[string, string][]} */
      const asked = [];
      const derive = (
        /** @type {string} */ tree,
        /** @type {string} */ log,
      ) => {
        asked.push([tree, log]);
        return derived;
      };
      assert.strictEqual(linkable(dir, "/tree", clang, derive), null);
      assert.deepStrictEqual(
        JSON.parse(readFileSync(join(dir, "notices.json"), "utf8")),
        derived,
      );
      // Asked of the entry's own seed log, in the tree the build names.
      assert.strictEqual(asked.length, 1);
      assert.strictEqual(asked[0][0], "/tree");
      assert.match(asked[0][1], /ninja/);
    });

    test("an entry whose record cannot be derived is not linked, and storing again adds it", () => {
      const { dir } = stored();
      unlinkSync(join(dir, "notices.json"));
      assert.match(
        linkable(dir, "/tree", clang, () => null) ?? "",
        /names no gn arguments/,
      );
      assert.match(
        linkable(dir, "/tree", clang, () => {
          throw new Error("gn gen failed");
        }) ?? "",
        /gn could not derive one: gn gen failed/,
      );
      assert.ok(!existsSync(join(dir, "notices.json")));
      const again = stored();
      assert.strictEqual(again.stored, false);
      assert.ok(existsSync(join(dir, "notices.json")));
      assert.strictEqual(
        linkable(dir, "/tree", clang, () => null),
        null,
      );
    });

    test("a build with other libraries is still refused", () => {
      writeFileSync(join(skia, "libskia.a"), "other");
      assert.throws(stored, /already holds other libraries/);
      writeFileSync(join(skia, "libskia.a"), "skia");
    });
  });

  test("a log naming no sysroot gives no key", () => {
    assert.match(loggedSdk("clang -c a.cpp", "darwin").why ?? "", /--sysroot/);
  });

  test("off macOS the SDK is none", () => {
    assert.deepStrictEqual(currentSdk({ SDKROOT: xcode }, "linux"), {
      sdk: "none",
    });
    assert.deepStrictEqual(loggedSdk("", "linux"), { sdk: "none" });
  });
});

// A fresh cargo home holds no registry, so `cargo tree --offline` fails
// before the build has fetched anything -- on every fresh CI runner, where
// every build then compiled Skia (#659). The tree is asked again after the
// target's crates are fetched. A stub cargo stands in for the real one: it
// fails the tree until a fetch has run, and records what the fetch asked.
describe("the features resolve before the build has fetched anything", async () => {
  const { skiaFeatures } = await import(
    pathToFileURL(join(__dirname, "..", "..", "scripts", "skia-binaries.mjs"))
      .href
  );
  const {
    mkdtempSync,
    readFileSync,
    existsSync,
    writeFileSync,
    chmodSync,
  } = require("fs");
  const { tmpdir } = require("os");
  const { delimiter } = require("path");

  /**
   * A cargo whose tree fails until a fetch has run, and whose fetch fails
   * where `fetchFails`, with the environment that puts it first on `PATH`.
   * @param {boolean} [fetchFails]
   */
  const stub = (fetchFails = false) => {
    const dir = mkdtempSync(join(tmpdir(), "skia-cache-fetch-"));
    const cargo = join(dir, "cargo");
    writeFileSync(
      cargo,
      [
        "#!/bin/sh",
        'case "$1" in',
        `  tree) [ -f "${dir}/fetched" ] && { echo "svg,binary-cache"; exit 0; }; exit 101 ;;`,
        `  fetch) echo "$@" > "${dir}/fetch-args"; ${fetchFails ? "exit 101" : `touch "${dir}/fetched"`} ;;`,
        "esac",
      ].join("\n"),
    );
    chmodSync(cargo, 0o755);
    return {
      dir,
      env: { ...process.env, PATH: `${dir}${delimiter}${process.env.PATH}` },
    };
  };
  const skip =
    process.platform === "win32" &&
    "the stub is a shell script, which Windows does not run as cargo";

  test(
    "an empty registry is fetched for the target, then resolved",
    { skip },
    () => {
      const { dir, env } = stub();
      const args = ["--target", "x86_64-unknown-linux-gnu"];
      assert.deepStrictEqual(skiaFeatures(args, dir, env), [
        "binary-cache",
        "svg",
      ]);
      assert.strictEqual(
        readFileSync(join(dir, "fetch-args"), "utf8").trim(),
        "fetch --locked --target x86_64-unknown-linux-gnu",
      );
    },
  );

  test("a registry that cannot be fetched resolves nothing", { skip }, () => {
    const { dir, env } = stub(true);
    assert.strictEqual(
      skiaFeatures(["--target", "x86_64-unknown-linux-gnu"], dir, env),
      null,
    );
    assert.ok(existsSync(join(dir, "fetch-args")), "the fetch was tried");
  });

  test("a registry already there is not fetched", { skip }, () => {
    const { dir, env } = stub();
    writeFileSync(join(dir, "fetched"), "");
    assert.deepStrictEqual(skiaFeatures([], dir, env), ["binary-cache", "svg"]);
    assert.ok(!existsSync(join(dir, "fetch-args")), "nothing was fetched");
  });
});

// A build through the script that misses the cache compiles Skia, and stores
// what it compiled so the next build of that key links it (#638). cargo
// names the out-dir and the features in its JSON messages, so such a build
// asks for them and passes everything else through.
describe("a build that misses the cache stores what it compiled", async () => {
  const { PassThrough } = require("stream");
  const { EventEmitter } = require("events");
  const { reporting, messageSplitter, skiaBuild, runCargo } = await import(
    pathToFileURL(SCRIPT).href
  );
  const FORMAT = "--message-format=json-render-diagnostics";
  const BINDINGS =
    "registry+https://github.com/rust-lang/crates.io-index#skia-bindings@0.156.0";
  const script = {
    reason: "build-script-executed",
    package_id: BINDINGS,
    out_dir: "/target/debug/build/skia-bindings/abc123/out",
  };
  const library = {
    reason: "compiler-artifact",
    package_id: BINDINGS,
    features: ["binary-cache", "metal"],
  };
  const other = {
    reason: "build-script-executed",
    package_id:
      "registry+https://github.com/rust-lang/crates.io-index#skia-bindings-extra@1.0.0",
    out_dir: "/elsewhere/out",
  };

  test("a building subcommand asks for cargo's messages, ahead of its own arguments", () => {
    assert.deepStrictEqual(
      reporting(["test", "--features", "metal", "--", "--nocapture"]),
      {
        args: ["test", FORMAT, "--features", "metal", "--", "--nocapture"],
        echo: false,
      },
    );
    assert.deepStrictEqual(reporting(["b", "-r"]), {
      args: ["b", FORMAT, "-r"],
      echo: false,
    });
  });

  test("a caller that asked for JSON itself is run as asked, and reads every line", () => {
    for (const args of [
      ["build", "--message-format=json-render-diagnostics", "--release"],
      ["build", "--message-format", "json"],
      ["build", "--message-format=short,json-diagnostic-short"],
    ])
      assert.deepStrictEqual(
        reporting(args),
        { args, echo: true },
        args.join(" "),
      );
  });

  test("a subcommand that builds nothing, or a format that is not JSON, is left alone", () => {
    for (const args of [
      ["tree"],
      ["fmt"],
      ["--version"],
      ["build", "--message-format=short"],
      ["build", "--message-format", "human"],
    ])
      assert.strictEqual(reporting(args), null, args.join(" "));
    // After `--` an argument is the test binary's, not cargo's.
    assert.deepStrictEqual(
      reporting(["test", "--", "--message-format=short"])?.args,
      ["test", FORMAT, "--", "--message-format=short"],
    );
  });

  test("cargo's messages are kept and every other byte is passed through, however it is cut", () => {
    const text = [
      "running 1 test\n",
      `${JSON.stringify(script)}\n`,
      "test a ... ",
      "ok\n",
      "{ not a message }\n",
      `${JSON.stringify({ no: "reason" })}\n`,
      `${JSON.stringify(library)}\n`,
      "",
    ].join("");
    for (const size of [1, 7, 64, text.length]) {
      let written = "";
      const split = messageSplitter((/** @type {string} */ part) => {
        written += part;
      });
      for (let at = 0; at < text.length; at += size)
        split.push(text.slice(at, at + size));
      split.end();
      assert.strictEqual(
        written,
        `running 1 test\ntest a ... ok\n{ not a message }\n${JSON.stringify({ no: "reason" })}\n`,
        `cut every ${size}`,
      );
      assert.deepStrictEqual(
        split.messages,
        [script, library],
        `cut every ${size}`,
      );
    }
  });

  test("output that does not open a line with `{` is written before the line ends", () => {
    /** @type {string[]} */
    const written = [];
    const split = messageSplitter((/** @type {string} */ part) =>
      written.push(part),
    );
    split.push("test a ... ");
    assert.deepStrictEqual(written, ["test a ... "]);
  });

  test("the build named is skia-bindings', not a package whose name extends it", () => {
    assert.deepStrictEqual(skiaBuild([other, script, library]), {
      buildDir: "/target/debug/build/skia-bindings/abc123",
      features: ["binary-cache", "metal"],
    });
    assert.strictEqual(skiaBuild([other, library]), null);
    assert.strictEqual(skiaBuild([script]), null);
  });

  // When every driven build starts: `store` refuses a build whose script
  // ran before it.
  const STARTED = 1234;

  /**
   * Runs `runCargo` against a cargo that writes `stdout` and exits with
   * `code`, the cache answering `cached`, and says what it ran and stored.
   * @param {string[]} args
   * @param {object} cached
   * @param {string} stdout
   * @param {{ code?: number, refuse?: string }} [options]
   */
  async function drive(args, cached, stdout, { code = 0, refuse } = {}) {
    /** @type {{ args: string[], options: any }[]} */
    const ran = [];
    /** @type {any[][]} */
    const saved = [];
    let written = "";
    const source = process.env.SKIA_SOURCE_DIR;
    process.env.SKIA_SOURCE_DIR = "/tree";
    try {
      const result = await runCargo(args, {
        spawn: (
          /** @type {string} */ _cargo,
          /** @type {string[]} */ argv,
          /** @type {any} */ options,
        ) => {
          ran.push({ args: argv, options });
          const child = Object.assign(new EventEmitter(), {
            stdout: options.stdio[1] === "pipe" ? new PassThrough() : null,
          });
          setImmediate(() => {
            if (child.stdout) child.stdout.end(stdout);
            setImmediate(() => child.emit("close", code, null));
          });
          return child;
        },
        cache: () => cached,
        save: (/** @type {any[]} */ ...stored) => {
          saved.push(stored);
          if (refuse) throw new Error(refuse);
        },
        write: (/** @type {string} */ text) => {
          written += text;
        },
        now: () => STARTED,
      });
      return { result, ran, saved, written };
    } finally {
      if (source === undefined) delete process.env.SKIA_SOURCE_DIR;
      else process.env.SKIA_SOURCE_DIR = source;
    }
  }

  const key = {
    tree: "t",
    target: "x",
    sdk: "s",
    compiler: { cc: "clang 1", cxx: "clang 1" },
    profile: "dev",
    features: ["metal"],
  };
  const messages = `${JSON.stringify(script)}\n${JSON.stringify(library)}\ntest a ... ok\n`;

  test("a miss with a key asks for the messages and stores the build cargo names", async () => {
    const { result, ran, saved, written } = await drive(
      ["build", "--features", "metal"],
      { why: "no entry", key },
      messages,
    );
    assert.deepStrictEqual(ran[0].args, [
      "build",
      FORMAT,
      "--features",
      "metal",
    ]);
    assert.deepStrictEqual(saved, [
      [
        "/target/debug/build/skia-bindings/abc123",
        {
          profile: "dev",
          features: ["binary-cache", "metal"],
          compiler: key.compiler,
          since: STARTED,
        },
      ],
    ]);
    assert.strictEqual(written, "test a ... ok\n");
    assert.strictEqual(result.code, 0);
  });

  test("a caller's own JSON reaches it whole, and the build is still stored", async () => {
    const { ran, saved, written } = await drive(
      ["build", "--message-format=json-render-diagnostics"],
      { why: "no entry", key },
      messages,
    );
    assert.deepStrictEqual(ran[0].args, [
      "build",
      "--message-format=json-render-diagnostics",
    ]);
    assert.strictEqual(written, messages);
    assert.strictEqual(saved.length, 1);
  });

  test("a failed command still stores the Skia its build script finished", async () => {
    const { result, saved } = await drive(
      ["test"],
      { why: "no entry", key },
      messages,
      {
        code: 101,
      },
    );
    assert.strictEqual(saved.length, 1);
    assert.strictEqual(result.code, 101);
  });

  test("a store that is refused leaves cargo's exit status as it was", async () => {
    const { result, saved } = await drive(
      ["build"],
      { why: "no entry", key },
      messages,
      {
        refuse: "the log names no patch",
      },
    );
    assert.strictEqual(saved.length, 1);
    assert.strictEqual(result.code, 0);
  });

  test("a hit links the entry and runs cargo as asked, storing nothing", async () => {
    const { ran, saved } = await drive(
      ["build"],
      { env: { SKIA_LIBRARY_SEARCH_PATH: "/entry" }, dir: "/entry", key },
      "",
    );
    assert.deepStrictEqual(ran[0].args, ["build"]);
    assert.strictEqual(ran[0].options.stdio[1], "inherit");
    assert.strictEqual(ran[0].options.env.SKIA_LIBRARY_SEARCH_PATH, "/entry");
    assert.deepStrictEqual(saved, []);
  });

  test("a miss with no key, or a subcommand that builds nothing, runs cargo as asked", async () => {
    for (const [args, cached] of [
      [["build"], { why: "SKIA_DEBUG is set" }],
      [["tree"], { why: "no entry", key }],
    ]) {
      const { ran, saved } = await drive(
        /** @type {string[]} */ (args),
        cached,
        "",
      );
      assert.deepStrictEqual(ran[0].args, args);
      assert.deepStrictEqual(saved, []);
    }
  });
});

// An entry's key names the compiler that built it, by the line its
// `--version` prints, so a clang update keys a new entry rather than
// linking the old clang's objects (#680). And two builds of one key are
// compared by the objects their archives hold: `ar` dates each member and
// `ranlib` its index, so their bytes never agree.
describe("an entry names its compiler, and two builds of one key are compared by their objects", async () => {
  const {
    archiveDifference,
    archiveMembers,
    compilerKey,
    entryDir,
    linkable,
    store,
  } = await import(
    pathToFileURL(join(__dirname, "..", "..", "scripts", "skia-binaries.mjs"))
      .href
  );
  const { treeName } = await import(pathToFileURL(SOURCE).href);
  const {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    realpathSync,
    rmSync,
    utimesSync,
    writeFileSync,
  } = require("fs");
  const { tmpdir } = require("os");

  const root = realpathSync(mkdtempSync(join(tmpdir(), "skia-cache-680-")));
  process.on("exit", () => rmSync(root, { recursive: true, force: true }));

  /**
   * The ar archive of `members`, `[name, contents]`, each header dated
   * `date`, behind a symbol index holding `index`: in BSD's form with every
   * name after its header, as Apple's `ar` and `libtool` write it, or System
   * V's with long names in a `//` table, as GNU `ar` writes it.
   * @param {[string, string][]} members
   * @param {{ date?: number, index?: string, form?: "bsd" | "sysv" }} [options]
   */
  function archive(members, { date = 0, index = "", form = "bsd" } = {}) {
    /** @type {Buffer[]} */
    const parts = [Buffer.from("!<arch>\n")];
    const add = (/** @type {string} */ name, /** @type {Buffer} */ body) => {
      const header = [
        name.padEnd(16),
        String(date).padEnd(12),
        "0".padEnd(6),
        "0".padEnd(6),
        "100644".padEnd(8),
        String(body.length).padEnd(10),
        "`\n",
      ].join("");
      parts.push(Buffer.from(header, "latin1"), body);
      if (body.length % 2) parts.push(Buffer.from("\n"));
    };
    if (form === "bsd") {
      const named = (
        /** @type {string} */ name,
        /** @type {string} */ text,
      ) => {
        const padded = Buffer.from(
          name.padEnd(name.length + 4 - (name.length % 4), "\0"),
        );
        add(`#1/${padded.length}`, Buffer.concat([padded, Buffer.from(text)]));
      };
      named("__.SYMDEF SORTED", index);
      for (const [name, text] of members) named(name, text);
    } else {
      add("/", Buffer.from(index));
      let table = "";
      /** @type {Map<string, number>} */
      const at = new Map();
      for (const [name] of members)
        if (name.length > 15) {
          at.set(name, table.length);
          table += `${name}/\n`;
        }
      if (table) add("//", Buffer.from(table));
      for (const [name, text] of members)
        add(at.has(name) ? `/${at.get(name)}` : `${name}/`, Buffer.from(text));
    }
    return Buffer.concat(parts);
  }

  /** @type {[string, string][]} */
  const objects = [
    ["xmlparse.o", "parser"],
    ["a-name-longer-than-sixteen.o", "odd"],
    ["xmlrole.o", "roles!"],
  ];
  const names = objects.map(([name]) => name);

  for (const form of /** @type {const} */ (["bsd", "sysv"]))
    test(`an archive's members are read in order, without the symbol index or the dates: ${form}`, () => {
      const one = archive(objects, { form, date: 1, index: "at one" });
      const two = archive(objects, { form, date: 2, index: "at two!" });
      assert.notDeepStrictEqual(one, two);
      const members = archiveMembers(one);
      assert.deepStrictEqual(
        members?.map((/** @type {any} */ m) => m.name),
        names,
      );
      assert.deepStrictEqual(archiveMembers(two), members);
      assert.strictEqual(archiveDifference(one, two), null);
    });

  test("a member's contents are what its digest covers", () => {
    const changed = objects.map(
      ([name, text]) =>
        /** @type {[string, string]} */ ([
          name,
          name === "xmlrole.o" ? "Roles!" : text,
        ]),
    );
    assert.strictEqual(
      archiveDifference(archive(objects), archive(changed, { date: 9 })),
      "its xmlrole.o differs",
    );
  });

  test("an archive with a member more, one fewer, or one renamed, differs at that member", () => {
    const base = archive(objects);
    assert.strictEqual(
      archiveDifference(base, archive([...objects, ["more.o", "x"]])),
      "this build's has more.o as well",
    );
    assert.strictEqual(
      archiveDifference(base, archive(objects.slice(0, 2))),
      "this build's has no xmlrole.o",
    );
    assert.strictEqual(
      archiveDifference(
        base,
        archive([objects[0], ["renamed.o", "odd"], objects[2]]),
      ),
      "member 2 is a-name-longer-than-sixteen.o in the entry and renamed.o in this build",
    );
  });

  test("bytes that are not an archive, or one cut short, read as none", () => {
    const whole = archive(objects);
    assert.strictEqual(archiveMembers(Buffer.from("skia")), null);
    assert.strictEqual(
      archiveMembers(whole.subarray(0, whole.length - 3)),
      null,
    );
    assert.strictEqual(archiveMembers(whole.subarray(0, 70)), null);
    assert.match(
      archiveDifference(whole, Buffer.from("skia")) ?? "",
      /this build's is not an ar archive/,
    );
  });

  test("archives the system's ar writes at two times hold one set of members", (t) => {
    const dir = join(root, "ar");
    mkdirSync(dir, { recursive: true });
    // Real objects: Apple's `ar` leaves out a member that is not Mach-O.
    const sources = ["a", "b"];
    for (const name of sources)
      writeFileSync(
        join(dir, `${name}.c`),
        `int ${name}(void) { return 1; }\n`,
      );
    const cc = spawnSync("cc", ["-c", ...sources.map((n) => `${n}.c`)], {
      cwd: dir,
    });
    if (cc.error || cc.status !== 0)
      return t.skip("no C compiler on this host");
    const members = sources.map((name) => `${name}.o`);
    // GNU ar writes zero dates unless asked with `U`; Apple's writes each
    // object's modification time.
    const flags = process.platform === "darwin" ? "rc" : "rcU";
    const made = [];
    for (const [i, when] of [1_000_000_000, 1_100_000_000].entries()) {
      for (const name of members) utimesSync(join(dir, name), when, when);
      const out = join(dir, `lib${i}.a`);
      const result = spawnSync("ar", [flags, out, ...members], { cwd: dir });
      if (result.error) return t.skip("no ar on this host");
      assert.strictEqual(result.status, 0, String(result.stderr));
      made.push(readFileSync(out));
    }
    assert.notDeepStrictEqual(
      made[0],
      made[1],
      "the dates did not reach the archive",
    );
    assert.deepStrictEqual(
      archiveMembers(made[0])?.map((/** @type {any} */ m) => m.name),
      members,
    );
    assert.strictEqual(archiveDifference(made[0], made[1]), null);
  });

  test("the compilers are those skia-bindings reads, clang where none is set", () => {
    /** @type {string[]} */
    const asked = [];
    const version = (/** @type {string} */ command) => {
      asked.push(command);
      return `${command} version 1`;
    };
    assert.deepStrictEqual(compilerKey({}, version), {
      compiler: { cc: "clang version 1", cxx: "clang++ version 1" },
    });
    assert.deepStrictEqual(asked, ["clang", "clang++"]);
    asked.length = 0;
    compilerKey({ CC: "cc-17", CLANGCC: "clang-18", CXX: "g++" }, version);
    assert.deepStrictEqual(asked, ["clang-18", "g++"]);
  });

  test("a compiler that cannot be named by its version gives no key", () => {
    const version = () => "clang version 1";
    assert.match(compilerKey({ CC: "" }, version).why ?? "", /CC is empty/);
    assert.match(
      compilerKey({ CXX: "ccache clang++" }, version).why ?? "",
      /CXX is "ccache clang\+\+", a command with arguments/,
    );
    assert.match(
      compilerKey({}, () => null).why ?? "",
      /`clang --version` named no version/,
    );
  });

  test("this host's compiler is named by the line its --version prints", (t) => {
    const { compiler, why } = compilerKey({});
    if (!compiler) return t.skip(`no clang here: ${why}`);
    const line = spawnSync("clang", ["--version"], { encoding: "utf8" })
      .stdout.split("\n")[0]
      .trim();
    assert.strictEqual(compiler.cc, line);
  });

  const key = {
    tree: "t",
    target: "aarch64-apple-darwin",
    sdk: "MacOSX27.2.sdk",
    profile: "dev",
    features: ["metal"],
  };
  const apple = "Apple clang version 21.0.0 (clang-2100.4.1.1)";

  test("two compilers give two entries, one clang's pair one", () => {
    const one = entryDir({ ...key, compiler: { cc: apple, cxx: apple } }, root);
    const next = "Apple clang version 21.0.0 (clang-2100.4.1.2)";
    assert.notStrictEqual(
      entryDir({ ...key, compiler: { cc: next, cxx: next } }, root),
      one,
    );
    assert.ok(
      one.includes(
        `${sep}Apple-clang-version-21.0.0-clang-2100.4.1.1${sep}dev${sep}`,
      ),
      one,
    );
    assert.ok(
      entryDir(
        { ...key, compiler: { cc: apple, cxx: "g++ 14" } },
        root,
      ).includes(
        `${sep}Apple-clang-version-21.0.0-clang-2100.4.1.1+g-14${sep}`,
      ),
    );
  });

  test("a version line cannot reach outside its directory", () => {
    const within = join(root, "binaries", "t", key.target, key.sdk);
    for (const [sly, name] of [
      ["../../../x", "..-..-..-x"],
      ["..\\..\\..\\x", "..-..-..-x"],
      ["C:\\x", "C-x"],
    ]) {
      const dir = entryDir({ ...key, compiler: { cc: sly, cxx: sly } }, root);
      assert.ok(dir.startsWith(within + sep), dir);
      assert.strictEqual(dir, join(within, name, "dev", "metal"), sly);
    }
  });

  test("a version line of dots alone names no compiler", () => {
    for (const line of ["..", ".", " .. ", "./.."]) {
      assert.match(
        compilerKey({}, () => line).why ?? "",
        /which names no version/,
        line,
      );
    }
  });

  // A build directory as skia-bindings leaves one: its log names the tree and
  // the SDK, and `out/skia` holds the libraries and the definitions.
  const cache = join(root, "cache");
  const sysroot = join(root, "MacOSX.sdk");
  mkdirSync(sysroot, { recursive: true });
  writeFileSync(join(sysroot, "SDKSettings.json"), '{"Version":"27.2"}');
  const compiler = { cc: apple, cxx: apple };

  /**
   * A build directory named `name` holding `libraries` and `defines`.
   * @param {string} name
   * @param {Record<string, Buffer>} libraries
   * @param {string} [defines]
   */
  function built(name, libraries, defines = "SK_X\n") {
    const dir = join(root, name);
    const skia = join(dir, "out", "skia");
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(skia, { recursive: true });
    writeFileSync(
      join(dir, "output"),
      `${join(cache, treeName())}/ninja\nclang --sysroot=${sysroot} -c a.cpp\n`,
    );
    for (const [library, bytes] of Object.entries(libraries))
      writeFileSync(join(skia, library), bytes);
    writeFileSync(join(skia, "skia-defines.txt"), defines);
    return dir;
  }

  /**
   * Stores `dir` as the entry for `compiler`, the build having started at
   * `since`.
   * @param {string} dir
   * @param {{ since?: number, by?: object }} [options]
   */
  const stored = (dir, { since = 0, by = compiler } = {}) =>
    store(
      dir,
      { profile: "dev", features: ["metal"], compiler: by, since },
      () => null,
      { cache, notices: () => ({}) },
    );

  const libskia = archive(objects, { date: 1, index: "first" });
  const again = archive(objects, { date: 2, index: "second" });

  test("a build whose script ran before the build started is not stored", () => {
    const dir = built("fresh", { "libskia.a": libskia });
    const long = Date.now() / 1000 - 3600;
    utimesSync(join(dir, "output"), long, long);
    assert.throws(
      () => stored(dir, { since: Date.now() }),
      /is not stored: its build script did not run in this build, so which compiler built it is not known/,
    );
    assert.strictEqual(stored(dir, { since: (long - 1) * 1000 }).stored, true);
  });

  test("a second build of one key holding the same objects is already held", () => {
    rmSync(cache, { recursive: true, force: true });
    const first = stored(
      built("first", { "libskia.a": libskia, "libsvg.a": again }),
    );
    assert.strictEqual(first.stored, true);
    assert.notDeepStrictEqual(libskia, again);
    const second = stored(
      built("second", { "libskia.a": again, "libsvg.a": libskia }),
    );
    assert.deepStrictEqual(second, { dir: first.dir, stored: false });
    assert.deepStrictEqual(readFileSync(join(first.dir, "libskia.a")), libskia);
  });

  /** @type {[string, Record<string, Buffer>, string | undefined, object | undefined, RegExp][]} */
  const refused = [
    [
      "an object that differs",
      {
        "libskia.a": libskia,
        "libsvg.a": archive([...objects.slice(0, 2), ["xmlrole.o", "other"]]),
      },
      undefined,
      undefined,
      /: libsvg\.a: its xmlrole\.o differs$/,
    ],
    [
      "a library more",
      { "libskia.a": libskia, "libsvg.a": again, "libzlib.a": again },
      undefined,
      undefined,
      /: this build has libzlib\.a, which the entry does not$/,
    ],
    [
      "a library fewer",
      { "libskia.a": libskia },
      undefined,
      undefined,
      /: this build has no libsvg\.a$/,
    ],
    [
      "other definitions",
      { "libskia.a": libskia, "libsvg.a": again },
      "SK_Y\n",
      undefined,
      /: its skia-defines\.txt differs$/,
    ],
    [
      "another compiler whose line names the same directory",
      { "libskia.a": libskia, "libsvg.a": again },
      undefined,
      { cc: apple.replace(" (", " ["), cxx: apple.replace(" (", " [") },
      /: it was compiled by \{"cc":"Apple clang version 21\.0\.0 \(clang/,
    ],
  ];
  for (const [label, libraries, defines, by, why] of refused)
    test(`a second build of one key is refused for ${label}`, () => {
      rmSync(cache, { recursive: true, force: true });
      stored(built("held", { "libskia.a": libskia, "libsvg.a": again }));
      assert.throws(
        () => stored(built("other", libraries, defines), { by }),
        (/** @type {Error} */ error) =>
          /already holds other libraries for the same key/.test(
            error.message,
          ) && why.test(error.message),
      );
    });

  test("an entry is linked only by the compiler its manifest names", () => {
    rmSync(cache, { recursive: true, force: true });
    const { dir } = stored(built("linked", { "libskia.a": libskia }));
    assert.ok(existsSync(join(dir, "notices.json")));
    assert.strictEqual(linkable(dir, "/tree", compiler), null);
    const other = { cc: apple.replace(" (", " ["), cxx: apple };
    assert.match(
      linkable(dir, "/tree", other) ?? "",
      /was compiled by \{"cc":"Apple clang version 21\.0\.0 \(clang/,
    );
  });
});

describe("a store reports beside cargo's messages, not among them", async () => {
  const { storeChecked } = await import(pathToFileURL(SCRIPT).href);

  // lib/prebuild.mjs hands a stored build's stdout to cargo-cp-artifact as
  // cargo's JSON stream, so a report written there fails the build after
  // Skia compiled.
  test("the report goes to stderr and stdout stays empty", () => {
    for (const stored of [true, false]) {
      let out = "";
      let err = "";
      const write = process.stdout.write;
      const error = console.error;
      process.stdout.write = (/** @type {any} */ text) => {
        out += String(text);
        return true;
      };
      console.error = (/** @type {any[]} */ ...parts) => {
        err += `${parts.join(" ")}\n`;
      };
      try {
        storeChecked(
          "/target/debug/build/skia-bindings/abc123/out",
          {},
          { put: () => ({ dir: "/cache/entry", stored }) },
        );
      } finally {
        process.stdout.write = write;
        console.error = error;
      }
      assert.strictEqual(out, "", `stdout, stored ${stored}`);
      assert.match(
        err,
        stored ? /skia-cache: stored \/cache\/entry/ : /already held/,
      );
    }
  });
});
