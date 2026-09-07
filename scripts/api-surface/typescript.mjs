// The pinned TypeScript, resolved once and checked before anything uses it.
//
// This directory pins 6.0.3, the last release whose `"."` export is the
// classic compiler API, while the root package has moved to 7.x. TypeScript 7
// resolves `"."` to `./lib/version.cjs`, an object with exactly two keys --
// `version` and `versionMajorMinor` -- so the first use of anything else
// throws `Cannot read properties of undefined (reading 'Latest')`, a message
// naming neither TypeScript, nor this directory, nor the install that is
// missing. Node resolution walks upward, so with
// `scripts/api-surface/node_modules` absent every consumer here silently gets
// the root's 7.x instead of the pin.
//
// That is a cold-checkout failure by construction: a worktree that has run
// `just check-parity` keeps the directory and never sees it, and CI is always
// cold. It reached four Test jobs across three operating systems as exactly
// that error before this guard existed.
import { createRequire } from "module";

const require = createRequire(import.meta.url);
const ts = require("typescript");

// Both halves are checked, and against 7.x either alone would do: its version
// stub carries neither `ScriptTarget` nor `createSourceFile`. The pair is
// caution rather than a case anyone has seen -- the failure being guarded is a
// wrong *module* rather than a wrong version, and a package could expose the
// enum without the parser or the reverse. Stated as caution because it is
// caution: an earlier version of this comment claimed 7.x had one and not the
// other, which is not true of any release measured.
if (
  ts.ScriptTarget === undefined ||
  typeof ts.createSourceFile !== "function"
) {
  throw new Error(
    `the TypeScript resolved here (version ${ts.version ?? "unknown"}) has no ` +
      "compiler API, which is absent above 6.x -- so the pin under " +
      "scripts/api-surface is not installed and resolution reached the root's " +
      "instead. Run " +
      "`bun install --cwd scripts/api-surface --frozen-lockfile`",
  );
}

export { ts };
