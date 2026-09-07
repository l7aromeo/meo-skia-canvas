// The pinned TypeScript, resolved once and checked before anything uses it.
//
// This directory pins 5.9.3 while the root package has moved to 7.x, whose
// point exports carry no compiler API: `ts.ScriptTarget` is `undefined` there,
// so the first use of it throws `Cannot read properties of undefined (reading
// 'Latest')` -- a message that names neither TypeScript, nor this directory,
// nor the install that is missing. Node resolution walks upward, so with
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

// `createSourceFile` is the entry point every caller here uses and
// `ScriptTarget` is the enum it takes; 7.x's point export has the first and
// not the second, so checking only for the module -- or only for a function --
// passes exactly the case this guards against.
if (
  ts.ScriptTarget === undefined ||
  typeof ts.createSourceFile !== "function"
) {
  throw new Error(
    `the TypeScript resolved here (version ${ts.version ?? "unknown"}) has no ` +
      "compiler API, which means the pin under scripts/api-surface is not " +
      "installed and resolution reached the root's instead -- run " +
      "`bun install --cwd scripts/api-surface --frozen-lockfile`",
  );
}

export { ts };
