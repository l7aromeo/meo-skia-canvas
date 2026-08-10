const fs = require("fs");
const { join } = require("path");
const ROOT = join(__dirname, "../..");

// lib.dom.d.ts is the reference for what the browser Canvas actually has.
// TypeScript 7 ships its libs in a platform package rather than under
// `typescript/lib`, so resolve it rather than hard-coding either location.
function domLibPath() {
  // TypeScript 5.x and earlier keep the libs here.
  const classic = join(ROOT, "node_modules/typescript/lib/lib.dom.d.ts");
  if (fs.existsSync(classic)) return classic;

  // TypeScript 7 ships them in a per-platform package. Read the directory
  // rather than listing platforms: an earlier version enumerated five and
  // omitted win32-arm64, so the Windows ARM leg found no reference at all.
  const scope = join(ROOT, "node_modules/@typescript");
  if (!fs.existsSync(scope)) return null;

  for (const entry of fs.readdirSync(scope)) {
    const candidate = join(scope, entry, "lib/lib.dom.d.ts");
    if (fs.existsSync(candidate)) return candidate;
  }
  return null;
}

const DOM_PATH = domLibPath();
const DOM = DOM_PATH ? fs.readFileSync(DOM_PATH, "utf8") : "";
const OURS = fs.readFileSync(join(ROOT, "lib/index.d.ts"), "utf8");

// Blank out everything inside parens and angle brackets, keeping newlines so
// line structure survives. Without this, a wrapped parameter list reads as a
// run of properties: `conicCurveTo(cpx, cpy, x, y, weight)` split over lines
// looks exactly like five members named cpx, cpy, x, y and weight.
function maskNested(src) {
  let out = "",
    depth = 0,
    prev = "";
  for (const c of src) {
    // The `>` of an arrow is not a closing bracket. Counting it as one closed
    // the mask early, so everything after a callback parameter fell through
    // unmasked: `toBlob(callback: (blob) => void, type?, quality?)` wrapped
    // across lines produced members named `type` and `quality`.
    const arrow = c === ">" && prev === "=";

    // Delimiters are kept -- `name(` is what marks a method -- and only the
    // contents between them are blanked.
    if (c === "(" || c === "<") {
      out += c;
      depth++;
    } else if ((c === ")" || c === ">") && !arrow) {
      depth = Math.max(0, depth - 1);
      out += c;
    } else {
      out += depth > 0 ? (c === "\n" ? "\n" : " ") : c;
    }

    prev = c;
  }
  return out;
}

function block(src, name) {
  const re = new RegExp(
    "^(?:export )?(?:declare )?(?:interface|class) " + name + "\\b([^{]*)\\{",
    "m",
  );
  const m = re.exec(src);
  if (!m) return null;
  let i = m.index + m[0].length,
    depth = 1,
    body = "";
  while (i < src.length && depth > 0) {
    const c = src[i];
    if (c === "{") depth++;
    else if (c === "}") depth--;
    if (depth > 0) body += c;
    i++;
  }
  const ext = (m[1].match(/extends\s+([^{]+)/) || [, ""])[1]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return { body, ext };
}

function members(src, name, seen = new Set()) {
  if (seen.has(name)) return new Set();
  seen.add(name);
  const b = block(src, name);
  if (!b) return new Set();
  const out = new Set();
  for (const m of maskNested(b.body).matchAll(
    /^\s*(?:readonly\s+|static\s+|get\s+|set\s+)*([A-Za-z_$][\w$]*)\s*[(:?]/gm,
  )) {
    // Not members: `prototype` is the constructor-object shape and
    // `constructor` is the declaration's own signature.
    if (m[1] !== "prototype" && m[1] !== "constructor") out.add(m[1]);
  }
  for (const parent of b.ext)
    for (const n of members(src, parent, seen)) out.add(n);
  return out;
}
// Our name for a standard DOM type, or null when the name collides with
// something unrelated -- our `Window` is a GUI window, not the browser global.
const CORRESPONDS_TO = {
  Canvas: "HTMLCanvasElement",
  Image: "HTMLImageElement",
  Window: null,
};

const MARK = "🧪";

/**
 * Every type the package puts in front of a consumer.
 *
 * Two shapes count. The obvious one is `export class` or `export interface`.
 * The other is the DOM house style this file borrows for the types lifted from
 * lib.dom.d.ts: a bare `interface X` paired with a `declare var X`, which is
 * exported by way of the variable rather than the interface.
 *
 * Matching only the first shape left six types unexamined -- DOMPoint, DOMRect,
 * DOMMatrix, CanvasGradient, Path2D and TextMetrics -- and with them 25
 * extensions that no test could see. Nineteen of Path2D's were unmarked.
 */
function exportedTypes() {
  const names = new Set(
    [
      ...OURS.matchAll(
        /^export (?:declare )?(?:class|interface) ([A-Za-z_$][\w$]*)/gm,
      ),
    ].map((m) => m[1]),
  );

  for (const [, name] of OURS.matchAll(/^declare var ([A-Za-z_$][\w$]*)/gm)) {
    // Only when the interface is actually there to compare against: a
    // `declare var` with no matching interface carries no members.
    if (new RegExp(`^interface ${name}\\b`, "m").test(OURS)) names.add(name);
  }

  return [...names];
}

/** Members of `name` that the browser equivalent does not have. */
function extensionsOf(name) {
  const mapped = name in CORRESPONDS_TO ? CORRESPONDS_TO[name] : name;
  const dom = mapped === null ? new Set() : members(DOM, mapped);
  if (dom.size === 0) return { wholeType: true, members: [] };
  const ours = members(OURS, name);
  return { wholeType: false, members: [...ours].filter((n) => !dom.has(n)) };
}

module.exports = {
  DOM,
  DOM_PATH,
  OURS,
  members,
  exportedTypes,
  extensionsOf,
  MARK,
};
