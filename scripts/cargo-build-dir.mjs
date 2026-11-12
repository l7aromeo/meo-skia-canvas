// Where cargo leaves a build script's run inside a target directory.
//
// cargo keeps each build-script run in a directory of its own under
// `<profile>/build` (or `<triple>/<profile>/build`), and the layout of that
// directory is internal to cargo. Two are in use:
//
// - cargo before 1.100: `build/<package>-<hash>/`, holding the run's stdout as
//   `output`, its stderr as `stderr`, and `out/`, the run's `OUT_DIR`.
// - cargo 1.100 and later: `build/<package>/<hash>/`, holding `run/stdout`,
//   `run/stderr` and `out/`.
//
// Both write `root-output` beside the logs, holding the run's `OUT_DIR`, so a
// unit directory is told from the other directories cargo keeps there -- the
// compiled build script, the package's own objects -- by that file rather than
// by its name. cargo's JSON `build-script-executed` message names `out_dir`,
// whose parent is the unit directory in both layouts; a caller that ran cargo
// itself should take it from there.

import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

/** The directories under `dir`, or none where `dir` does not exist. */
function directories(dir) {
  try {
    return readdirSync(dir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name);
  } catch (error) {
    if (error.code === "ENOENT" || error.code === "ENOTDIR") return [];
    throw error;
  }
}

/**
 * Where a build script's run left its stdout and stderr in `dir`, the unit
 * directory that holds its `out`: `run/stdout` and `run/stderr` in cargo 1.100's
 * layout, `output` and `stderr` in the layout before it. A directory holding
 * both is read as the older layout, which is the one cargo wrote `output` for.
 */
export function buildScriptLogs(dir) {
  const run = join(dir, "run");
  return existsSync(join(run, "stdout")) && !existsSync(join(dir, "output"))
    ? { output: join(run, "stdout"), stderr: join(run, "stderr") }
    : { output: join(dir, "output"), stderr: join(dir, "stderr") };
}

/** Whether `dir` is a unit directory a build script ran in, in either layout. */
function ran(dir) {
  return (
    existsSync(join(dir, "root-output")) ||
    existsSync(join(dir, "run", "root-output"))
  );
}

/**
 * The unit directories under `build`, a `<profile>/build` directory, in which
 * `pkg`'s build script ran: `build/<pkg>-<hash>` before cargo 1.100 and
 * `build/<pkg>/<hash>` from it. A package whose name extends `pkg`'s
 * (`skia-bindings-extra` beside `skia-bindings`) is not `pkg`'s: a hash is
 * hexadecimal and has no `-`.
 */
export function buildScriptRuns(build, pkg) {
  const older = directories(build)
    .filter((name) => name.startsWith(`${pkg}-`))
    .filter((name) => /^[0-9a-f]+$/.test(name.slice(pkg.length + 1)))
    .map((name) => join(build, name));
  const newer = directories(join(build, pkg)).map((hash) =>
    join(build, pkg, hash),
  );
  return [...older, ...newer].filter(ran);
}

/**
 * The `build` directories of a target directory: `<profile>/build` for the
 * host and `<triple>/<profile>/build` for a named target.
 */
export function profileBuildDirs(target) {
  return directories(target).flatMap((first) => [
    join(target, first, "build"),
    ...directories(join(target, first)).map((second) =>
      join(target, first, second, "build"),
    ),
  ]);
}
