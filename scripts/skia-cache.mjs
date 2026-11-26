#!/usr/bin/env node
// A local cache of compiled, patched Skia, for every worktree to link
// instead of compiling it again. What an entry is, and why it is linked the
// way it is, is in scripts/skia-binaries.mjs.
//
//   node scripts/skia-cache.mjs cargo [+toolchain] <cargo args>  cargo, linking the cached Skia where an entry fits, storing what it compiles where none does
//   node scripts/skia-cache.mjs fill [cargo feature and profile args]  build Skia once and store it
//   node scripts/skia-cache.mjs entry [cargo feature and profile args]  the entry those args would use
//
// `cargo`, `fill` and `entry` refuse where `SKIA_SOURCE_DIR` names no tree
// and `node scripts/skia-source.mjs env` would name one: skia-bindings would
// then build against rust-skia's unpatched Skia, and every patch test would
// expect stock Skia and pass. `SKIA_CACHE_UNPATCHED=1` asks for that build on
// purpose. Every `just` recipe sets the variable through its dotenv command.
import { execFileSync, spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { cacheEnv, compilerKey, readArgs, store } from "./skia-binaries.mjs";
import { PATCHED, refusal } from "./check-patched-skia.mjs";
import { namesSourceTree } from "./skia-source.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// The variable that asks for a build against unpatched Skia, and the one
/// value that asks: anything else set there is a mistake, and refused.
const UNPATCHED = "SKIA_CACHE_UNPATCHED";
const UNPATCHED_ASKED = "1";

/// What sets `SKIA_SOURCE_DIR` outside `just`, as the refusal names it.
const SETS_SOURCE_DIR = `eval "$(node scripts/skia-source.mjs env | sed 's/^/export /')"`;

/**
 * Why a build with `env` would link Skia nobody asked for, or `null` when it
 * would not. `namesTree` is whether this checkout names a tree at all -- see
 * `namesSourceTree` -- and where it does not, skia-bindings' own choice is
 * the right one.
 */
export function unpatchedRefusal(
  env = process.env,
  namesTree = namesSourceTree(),
) {
  const asked = env[UNPATCHED];
  if (asked !== undefined && asked !== UNPATCHED_ASKED)
    return `skia-cache: ${UNPATCHED} is ${JSON.stringify(asked)}; set it to ${UNPATCHED_ASKED} to build unpatched Skia, or unset it.`;
  const dir = env.SKIA_SOURCE_DIR;
  if (asked === UNPATCHED_ASKED)
    return dir
      ? `skia-cache: ${UNPATCHED}=${UNPATCHED_ASKED} asks for unpatched Skia, but SKIA_SOURCE_DIR names ${dir}. Unset one of them.`
      : null;
  if (dir || !namesTree) return null;
  return [
    `skia-cache: SKIA_SOURCE_DIR ${dir === "" ? "is empty" : "is not set"}, so skia-bindings would build against rust-skia's unpatched Skia.`,
    `Run through \`just\`, or first: ${SETS_SOURCE_DIR}`,
    `To build unpatched Skia on purpose, set ${UNPATCHED}=${UNPATCHED_ASKED}.`,
  ].join("\n");
}

/**
 * Throws `unpatchedRefusal`'s reason, and says so on stderr where a build of
 * unpatched Skia was asked for.
 */
function refuseUnpatched() {
  const why = unpatchedRefusal();
  if (why) throw new Error(why);
  if (process.env[UNPATCHED] === UNPATCHED_ASKED && namesSourceTree())
    console.error(
      `skia-cache: building unpatched Skia (${UNPATCHED}=${UNPATCHED_ASKED})`,
    );
}

/// cargo's subcommands, and their aliases, that build and take
/// `--message-format`: through one of them a build that compiles Skia can
/// name, in cargo's JSON, the out-dir it compiled in and the features it
/// compiled with, so that it is stored.
const REPORTING = new Set([
  ...["build", "b", "check", "c", "clippy", "test", "t", "bench"],
  ...["run", "r", "rustc", "rustdoc", "doc", "d"],
]);

/// The format a build that is to be stored runs with: diagnostics rendered
/// on stderr as without it, and cargo's messages as JSON on stdout.
const MESSAGES = "--message-format=json-render-diagnostics";

/**
 * How cargo given `args`, a subcommand and its arguments, is run so that its
 * JSON messages can be read: `args` asking for them, or as they are where
 * the caller asked for JSON itself -- `lib/prebuild.mjs` does, for
 * cargo-cp-artifact -- in which case `echo` says every line, messages
 * included, is the caller's to read. `null` where the subcommand builds
 * nothing that could be stored, or the caller chose a format that is not
 * JSON.
 */
export function reporting(args) {
  const [command, ...rest] = args;
  if (!REPORTING.has(command)) return null;
  const end = rest.indexOf("--");
  const own = end === -1 ? rest : rest.slice(0, end);
  const at = own.findIndex((arg) => arg.startsWith("--message-format"));
  if (at === -1) return { args: [command, MESSAGES, ...rest], echo: false };
  const format = own[at].includes("=")
    ? own[at].slice(own[at].indexOf("=") + 1)
    : (own[at + 1] ?? "");
  return format.split(",").some((kind) => kind.startsWith("json"))
    ? { args, echo: true }
    : null;
}

/**
 * Splits what cargo writes on stdout into its JSON messages, kept in
 * `messages`, and everything else -- a test's output, a program's -- passed
 * to `write` as it arrives. A line is a message when it parses as an object
 * with a `reason`, cargo's field for what a message reports; only a line
 * that opens with `{` is held back until it ends, so other output is not
 * delayed.
 */
export function messageSplitter(write) {
  const messages = [];
  let held = "";
  let passing = false;
  const parsed = (line) => {
    try {
      return JSON.parse(line);
    } catch {
      return null;
    }
  };
  const settle = (line) => {
    const message = parsed(line);
    if (typeof message?.reason === "string") messages.push(message);
    else write(line);
  };
  return {
    messages,
    push(chunk) {
      let text = chunk;
      while (text) {
        const end = text.indexOf("\n");
        const part = end === -1 ? text : text.slice(0, end + 1);
        text = end === -1 ? "" : text.slice(end + 1);
        if (passing || (!held && !part.startsWith("{"))) {
          write(part);
          passing = !part.endsWith("\n");
        } else {
          held += part;
          if (held.endsWith("\n")) {
            settle(held);
            held = "";
          }
        }
      }
    },
    end() {
      if (held) settle(held);
      held = "";
    },
  };
}

/**
 * The unit directory skia-bindings' build script ran in, and the features
 * skia-bindings was compiled with, as cargo's JSON `messages` name them, or
 * `null` where they name neither.
 */
export function skiaBuild(messages) {
  const isBindings = (m) => /[/#]skia-bindings[@#]/.test(m.package_id ?? "");
  const script = messages.find(
    (m) => m.reason === "build-script-executed" && isBindings(m),
  );
  const library = messages.find(
    (m) => m.reason === "compiler-artifact" && isBindings(m),
  );
  if (!script || !library) return null;
  return { buildDir: dirname(script.out_dir), features: library.features };
}

/**
 * Stores the Skia a build compiled, from cargo's `messages`, through `save`,
 * as the entry of `key`'s profile and compiler -- the build having started
 * at `since` -- and says on stderr why where it does not. A refusal does not
 * fail the build: the cargo command asked for succeeded or failed on its own
 * terms, and the next build compiles again.
 */
function keep(messages, { profile, compiler }, since, save) {
  const build = skiaBuild(messages);
  if (!build) {
    console.error(
      "skia-cache: nothing stored: cargo reported no skia-bindings build",
    );
    return;
  }
  try {
    save(build.buildDir, {
      profile,
      features: build.features,
      compiler,
      since,
    });
  } catch (error) {
    console.error(`skia-cache: not stored: ${error.message}`);
  }
}

/**
 * Runs cargo with `args`, linking the cached Skia where an entry fits, and
 * says which on stderr. A miss builds as cargo always does, and where the
 * args have a key and the subcommand reports, stores the Skia it compiled --
 * see `reporting` -- so the next build of that key links it. Resolves to how
 * cargo exited. `spawn`, `cache`, `save` and `now` are cargo, `cacheEnv`,
 * `storeChecked` and `Date.now` but in a test.
 *
 * A stored build runs with cargo's stdout through this process, so that the
 * messages naming the out-dir can be read; a test harness writing there sees
 * a pipe rather than a terminal, and colours nothing.
 */
export function runCargo(
  args,
  {
    spawn: run = spawn,
    cache = cacheEnv,
    save = storeChecked,
    write = (text) => process.stdout.write(text),
    now = Date.now,
  } = {},
) {
  refuseUnpatched();
  const toolchain = args[0]?.startsWith("+") ? [args[0]] : [];
  const rest = args.slice(toolchain.length);
  const since = now();
  const { env, dir, why, key } = cache(rest);
  const asked = env || !key ? null : reporting(rest);
  console.error(
    env
      ? `skia-cache: linking ${dir}`
      : `skia-cache: compiling Skia (${why})${asked ? ", to store" : ""}`,
  );
  const child = run("cargo", [...toolchain, ...(asked?.args ?? rest)], {
    stdio: ["inherit", asked ? "pipe" : "inherit", "inherit"],
    env: { ...process.env, ...env },
  });
  const split = asked ? messageSplitter(asked.echo ? () => {} : write) : null;
  if (split) {
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (asked.echo) write(chunk);
      split.push(chunk);
    });
  }
  return new Promise((done) =>
    child.on("close", (code, signal) => {
      if (split) {
        split.end();
        keep(split.messages, key, since, save);
      }
      done({ code, signal });
    }),
  );
}

/**
 * Builds the library with `args` from source, the cache unused, and stores
 * the Skia it compiled. cargo names the out-dir it ran skia-bindings' build
 * script in, and the features it built skia-bindings with, in its JSON
 * messages. Where cargo finds that build fresh, nothing is stored -- see
 * `store` -- and `cargo clean -p skia-bindings` with the same profile makes
 * the next fill compile it.
 */
function fill(args) {
  refuseUnpatched();
  const read = readArgs(args);
  if (!read) throw new Error("skia-cache: fill builds for the host only");
  const { compiler, why } = compilerKey();
  if (!compiler) throw new Error(`skia-cache: nothing to fill: ${why}`);
  const since = Date.now();
  const env = { ...process.env };
  for (const name of ["SKIA_LIBRARY_SEARCH_PATH", "SKIA_BUILD_DEFINES"])
    delete env[name];
  const json = execFileSync(
    "cargo",
    ["build", "--lib", "--message-format=json-render-diagnostics", ...args],
    {
      cwd: ROOT,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "inherit"],
      maxBuffer: 1 << 30,
    },
  );
  const split = messageSplitter(() => {});
  split.push(json);
  split.end();
  const build = skiaBuild(split.messages);
  if (!build) throw new Error("skia-cache: cargo built no skia-bindings");
  storeChecked(build.buildDir, {
    profile: read.profile,
    features: build.features,
    compiler,
    since,
  });
}

/**
 * `store`, vouched for by check-patched-skia's own reading of the log;
 * `build` is what `store` takes.
 */
function storeChecked(buildDir, build) {
  const patched = PATCHED[process.platform];
  if (!patched)
    throw new Error(`skia-cache: no patches for ${process.platform}`);
  const { dir, stored } = store(buildDir, build, (log) =>
    refusal(log, patched),
  );
  console.log(`skia-cache: ${stored ? "stored" : "already held"} ${dir}`);
}

const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (main) {
  const [command, ...args] = process.argv.slice(2);
  try {
    if (command === "cargo")
      runCargo(args).then(({ code, signal }) => {
        if (signal) process.kill(process.pid, signal);
        process.exit(code ?? 1);
      });
    else if (command === "fill") fill(args);
    else if (command === "entry") {
      refuseUnpatched();
      const { dir, why } = cacheEnv(args);
      console.log(dir ?? `none: ${why}`);
    } else {
      console.error("usage: node scripts/skia-cache.mjs cargo|fill|entry ...");
      process.exit(2);
    }
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
