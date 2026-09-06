//
// Fails when `lib/index.d.ts` and the built addon disagree about what exists.
//
// Thirteen defects of this shape were shipping when it was first run, and no
// gate in this repository could see any of them: `locale` and `strokeWidth`
// accepted by the paragraph parser and declared nowhere, `ImageData.prototype`
// declared and absent. Nothing catches these because each half is internally
// consistent -- `tsc` believes the declarations and the tests exercise the
// runtime, and neither asks whether the two describe the same library.
//
// Both directions fail. A member the runtime has and the declarations lack is
// invisible to every TypeScript caller; a member declared and absent is a lie
// to them. The audit found defects in both.
//
// Usage:  node check.mjs <repo root>
//
import { createRequire } from "module";
import { runtimeSurface, declaredSurface } from "./surface.mjs";
import { ALLOWED } from "./allowed.mjs";

const root = process.argv[2];
if (!root) {
  process.stderr.write("usage: node check.mjs <repo root>\n");
  process.exit(2);
}

const require = createRequire(`${root}/package.json`);
const mod = require(`${root}/lib/index.js`);

// Constructed so their own instance properties can be read: the binding puts
// some state on the instance, and a walk of the prototypes alone cannot see
// it. `TextMetrics` is here because its declaration describes a measurement
// rather than the bare constructor the module exports -- without a sample its
// fourteen declared members all read as missing.
const SAMPLES = {
  Canvas: () => new mod.Canvas(2, 2),
  Path2D: () => new mod.Path2D(),
  DOMMatrix: () => new mod.DOMMatrix(),
  DOMPoint: () => new mod.DOMPoint(0, 0),
  DOMRect: () => new mod.DOMRect(0, 0, 1, 1),
  ImageData: () => new mod.ImageData(1, 1),
  Image: () => new mod.Image(),
  CanvasRenderingContext2D: () => new mod.Canvas(2, 2).getContext("2d"),
  TextMetrics: () => new mod.Canvas(2, 2).getContext("2d").measureText("x"),
};

const runtime = runtimeSurface(mod, SAMPLES);
const declared = declaredSurface(`${root}/lib/index.d.ts`, [
  // `Image`, `App` and `Window` extend `EventEmitter` from "stream". Without
  // this the whole emitter surface reports as undeclared on all three -- 45
  // rows, fifteen each, every one against declarations that compile clean.
  `${root}/node_modules/@types/node/events.d.ts`,
]);

const exempt = new Map(ALLOWED.map((entry) => [entry.member, entry.why]));
const unused = new Set(exempt.keys());
const undeclared = [];
const missing = [];
const unclaimed = [];

for (const [holder, keys] of runtime) {
  if (!declared.has(holder)) {
    unclaimed.push(holder);
    continue;
  }
  const claimed = declared.closed(holder);
  for (const key of keys) {
    const path = `${holder}.${key}`;
    if (exempt.has(path)) unused.delete(path);
    else if (!claimed.has(key)) undeclared.push(path);
  }
  for (const key of claimed)
    if (!keys.has(key)) missing.push(`${holder}.${key}`);
}

const report = [];
if (unclaimed.length)
  report.push(
    `${unclaimed.length} export(s) the declarations do not mention at all:\n` +
      unclaimed.map((n) => `    ${n}`).join("\n"),
  );
if (undeclared.length)
  report.push(
    `${undeclared.length} member(s) reachable at runtime and undeclared:\n` +
      undeclared
        .sort()
        .map((n) => `    ${n}`)
        .join("\n") +
      "\n  Declare them in lib/index.d.ts, or add them to allowed.mjs with the" +
      "\n  reason they are deliberate.",
  );
if (missing.length)
  report.push(
    `${missing.length} member(s) declared and absent at runtime:\n` +
      missing
        .sort()
        .map((n) => `    ${n}`)
        .join("\n") +
      "\n  A TypeScript caller is being promised something that is not there.",
  );
// An exemption for something that no longer exists is stale, and a stale
// exemption is how a list like this stops being read. It fails the same way
// the thing it exempts would.
if (unused.size)
  report.push(
    `${unused.size} allowlist entr(ies) naming nothing reachable:\n` +
      [...unused]
        .sort()
        .map((n) => `    ${n}`)
        .join("\n") +
      "\n  Remove them from allowed.mjs -- what they exempted is gone.",
  );

if (report.length) {
  process.stderr.write(
    `lib/index.d.ts and the built addon disagree.\n\n  ${report.join("\n\n  ")}\n`,
  );
  process.exit(1);
}

process.stdout.write(
  `api-surface: ${runtime.size} holders agree, ${exempt.size} exempt by name\n`,
);
