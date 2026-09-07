//
// A TOML reader for exactly the manifest shape `PARITY-SPEC.md` fixes, and
// nothing else.
//
// WHY NOT A GENERAL PARSER. There is no TOML in Node and none in this tree's
// dependencies, so the choice is a new dependency or a small reader. The
// small reader is the safer half of that trade only if it is **fail-closed**:
// a lenient one is itself a way for the manifest to rot in silence. A
// mis-keyed `rusr = [...]` ignored by a forgiving parser leaves the entry
// with an empty Rust side, and if it carries a `why` the gate reads it as a
// deliberate single-surface decision and passes. The typo would have removed
// a capability from the register without failing anything.
//
// So every line must be one of the forms below. Anything else throws, naming
// the line and its number.
//
const KEYS = new Set(["name", "rust", "npm", "why"]);

/** Parses the manifest subset: `[[capability]]` tables of strings and arrays. */
export function parseManifest(text, path = "manifest") {
  const entries = [];
  let current = null;
  let pendingKey = null;
  let pendingValue = "";

  const finishKey = (lineNo) => {
    if (pendingKey === null) return;
    const raw = pendingValue.trim();
    let value;
    if (raw.startsWith("[")) {
      if (!raw.endsWith("]")) {
        throw new Error(
          `${path}:${lineNo}: unterminated array for '${pendingKey}'`,
        );
      }
      const inner = raw.slice(1, -1).trim();
      value =
        inner === ""
          ? []
          : inner.split(",").map((part, i) => {
              const s = part.trim();
              if (!/^"[^"]*"$/.test(s)) {
                throw new Error(
                  `${path}:${lineNo}: array element ${i} of '${pendingKey}' is not a quoted string: ${s}`,
                );
              }
              return s.slice(1, -1);
            });
    } else if (/^"[^"]*"$/.test(raw)) {
      value = raw.slice(1, -1);
    } else {
      throw new Error(
        `${path}:${lineNo}: value for '${pendingKey}' is not a quoted string or array: ${raw}`,
      );
    }
    if (pendingKey in current) {
      throw new Error(
        `${path}:${lineNo}: '${pendingKey}' given twice in one capability`,
      );
    }
    current[pendingKey] = value;
    pendingKey = null;
    pendingValue = "";
  };

  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const lineNo = i + 1;
    const line = lines[i];
    const trimmed = line.trim();

    // A comment ends a pending value rather than continuing it. Without this
    // the continuation branch below appends the comment to the value, and
    // since a finished `why` already carries its closing quote the result is
    // refused as `not a quoted string` -- so a comment between two entries
    // fails the whole manifest, naming a line that is not the problem.
    //
    // Ending the value here keeps the reader fail-closed where it matters: a
    // comment in the MIDDLE of a value still throws, because what it
    // terminates is then genuinely unterminated.
    if (pendingKey !== null && trimmed.startsWith("#")) {
      finishKey(lineNo - 1);
      continue;
    }

    if (
      pendingKey !== null &&
      !/^[A-Za-z_]+\s*=/.test(trimmed) &&
      trimmed !== "[[capability]]"
    ) {
      // A continuation of the previous value: `why` wraps across lines.
      pendingValue += " " + trimmed;
      continue;
    }
    finishKey(lineNo - 1);

    if (trimmed === "" || trimmed.startsWith("#")) continue;

    if (trimmed === "[[capability]]") {
      current = {};
      entries.push(current);
      continue;
    }

    const eq = trimmed.indexOf("=");
    if (eq === -1) {
      throw new Error(
        `${path}:${lineNo}: not a key, a comment, or [[capability]]: ${trimmed}`,
      );
    }
    const key = trimmed.slice(0, eq).trim();
    if (!KEYS.has(key)) {
      throw new Error(
        `${path}:${lineNo}: unknown key '${key}'. Allowed: ${[...KEYS].join(", ")}. ` +
          `A misspelled key would silently empty a side, so it is refused rather than ignored.`,
      );
    }
    if (current === null) {
      throw new Error(
        `${path}:${lineNo}: '${key}' appears before any [[capability]]`,
      );
    }
    pendingKey = key;
    pendingValue = trimmed.slice(eq + 1);
  }
  finishKey(lines.length);

  entries.forEach((entry, i) => {
    for (const required of ["name", "rust", "npm"]) {
      if (!(required in entry)) {
        throw new Error(`${path}: capability ${i + 1} has no '${required}'`);
      }
    }
    if (typeof entry.name !== "string") {
      throw new Error(`${path}: capability ${i + 1} 'name' must be a string`);
    }
    for (const side of ["rust", "npm"]) {
      if (!Array.isArray(entry[side])) {
        throw new Error(
          `${path}: capability '${entry.name}' has a '${side}' that is not an array`,
        );
      }
    }
  });
  return entries;
}
