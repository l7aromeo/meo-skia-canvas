#!/usr/bin/env node
//
// The parameter pass of the parity gate, report only.
//
// The item gate passes `loadImage` against `Image::from_encoded` because the
// names pair; one takes a URL and the other only bytes. This pass holds each
// parameter of a JavaScript method to the Rust function it pairs with: a
// parameter JavaScript accepts that no candidate Rust function takes is
// `missing`, and one whose accepted values the Rust side covers only in part
// -- `string | URL` against `&[u8]` -- is `narrower`.
//
// THE CANDIDATES for a JavaScript method are the Rust functions it pairs with
// by name, exactly as the gate pairs them (`nameMaps`); the siblings of those
// in the same holder whose name extends theirs, which is how the crate spells
// a wider form -- `save_layer` and `save_layer_with`, `draw_image` and
// `draw_image_region`; and the Rust functions of a two-sided entry naming the
// method. Each overload is held to whichever candidate covers most of it,
// matching a parameter by name first and then by position, with a compatible
// class either way. A method with no candidate is the item gate's business,
// not this pass's.
//
// REPORT ONLY. It prints what it finds and exits 0 unless its own inputs are
// broken. Its first run measures this instrument as much as the surfaces, so
// the rows are triaged before anything is allowed to fail on them.
//
// Usage: node scripts/check-parity-params.mjs [--self-test | --summary]
//   reads target/parity-rust.json, target/parity-npm.json,
//   target/parity-params.json and parity.toml; `just report-parity-params`
//   writes all four and prints every row.

import { readFileSync } from "node:fs";
import { nameMaps } from "./check-parity.mjs";
import { parseManifest } from "./parity/toml.mjs";

/** `fillRule` -> `fill_rule`; `dx` stays `dx`. */
const snake = (name) =>
  name
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();

/**
 * The JavaScript classes a Rust class takes, beyond its own: the same values
 * under another spelling. A Rust enum takes JavaScript's string literals and
 * also the numeric codes the CanvasKit-shaped declarations use for one. A
 * struct and a list take each other, because JavaScript spells a point, a
 * rectangle or a size as a tuple where the crate names a struct -- the
 * coarsest of these, and the one most able to hide a real difference.
 */
const ACCEPTS = {
  string: ["string", "enum", "path"],
  enum: ["enum", "string", "number"],
  path: ["path", "string"],
  number: ["number"],
  boolean: ["boolean"],
  bytes: ["bytes"],
  array: ["array", "object"],
  object: ["object", "array"],
  callback: ["callback"],
  range: ["number"],
};

/** The JavaScript classes `rust` covers, out of `js`. */
const covered = (js, rust) =>
  rust.includes("unknown") || js.includes("unknown")
    ? js
    : js.filter((c) => rust.some((r) => (ACCEPTS[r] ?? [r]).includes(c)));

/**
 * How one JavaScript overload maps onto one Rust function: every name match
 * first, then the remaining parameters by position. One pass doing both lets
 * an earlier parameter take, by position, the slot a later one names -- `x`
 * claiming `max_width` before `maxWidth` is reached.
 */
function fit(overload, fn) {
  const taken = new Set();
  const slot = new Array(overload.params.length).fill(-1);
  overload.params.forEach((p, at) => {
    const i = fn.params.findIndex(
      (r, k) =>
        !taken.has(k) &&
        (r.name === snake(p.name) || r.name === p.name) &&
        covered(p.classes, r.classes).length > 0,
    );
    if (i !== -1) {
      slot[at] = i;
      taken.add(i);
    }
  });
  // Then by position, as a cursor that follows the last slot used rather
  // than the JavaScript index, so one Rust parameter standing for two does
  // not shift every one after it. That is a `Range`: two JavaScript numbers,
  // `start` and `end`, and the second takes the slot the first did.
  let previous = -1;
  overload.params.forEach((p, at) => {
    if (slot[at] !== -1) {
      previous = slot[at];
      return;
    }
    if (
      previous !== -1 &&
      fn.params[previous].classes.includes("range") &&
      p.classes.includes("number") &&
      slot.filter((s) => s === previous).length === 1
    ) {
      slot[at] = previous;
      return;
    }
    const next = fn.params.findIndex((_, k) => k > previous && !taken.has(k));
    if (next === -1) return;
    if (covered(p.classes, fn.params[next].classes).length > 0) {
      slot[at] = next;
      taken.add(next);
      previous = next;
    }
  });
  const outcome = overload.params.map((p, at) => {
    if (slot[at] === -1) return { param: p, rust: null, lost: p.classes };
    const r = fn.params[slot[at]];
    const kept = covered(p.classes, r.classes);
    return {
      param: p,
      rust: r,
      lost: p.classes.filter((c) => !kept.includes(c)),
    };
  });
  return { score: outcome.filter((o) => o.rust).length, outcome };
}

/** Every row the pass reports, as `{ npm, overload, param, kind, rust, detail }`. */
export function paramReport({ rust, npm, manifest, rules, params }) {
  const { rustNames, npmNames } = nameMaps({ rust, npm, rules });
  const byName = new Map();
  for (const [id, names] of rustNames) {
    if (!params.rust[id]) continue;
    for (const n of names) byName.set(n, [...(byName.get(n) ?? []), id]);
  }
  const rustFns = Object.keys(params.rust);
  const rows = [];
  const compared = new Set();
  for (const [id, record] of Object.entries(params.npm)) {
    const paired = new Set();
    for (const n of npmNames.get(id) ?? [])
      for (const r of byName.get(n) ?? []) paired.add(r);
    for (const entry of manifest) {
      if (entry.rust.length && entry.npm.includes(id)) {
        for (const r of entry.rust) if (params.rust[r]) paired.add(r);
      }
    }
    for (const r of [...paired]) {
      const prefix = `${r}_`;
      for (const f of rustFns) if (f.startsWith(prefix)) paired.add(f);
    }
    if (paired.size === 0) continue;
    compared.add(id);
    record.overloads.forEach((overload, at) => {
      let best = null;
      for (const r of [...paired].sort()) {
        const f = fit(overload, params.rust[r]);
        if (!best || f.score > best.score) best = { ...f, rust: r };
      }
      for (const o of best.outcome) {
        if (!o.rust) {
          rows.push({
            npm: id,
            overload: at,
            param: o.param.name,
            kind: "missing",
            rust: best.rust,
            detail: `${o.param.classes.join(" | ")}${o.param.optional ? ", optional" : ""}`,
          });
        } else if (o.lost.length > 0) {
          rows.push({
            npm: id,
            overload: at,
            param: o.param.name,
            kind: "narrower",
            rust: `${best.rust}(${o.rust.name})`,
            detail: `${o.lost.join(" | ")} not taken; Rust has ${o.rust.classes.join(" | ")}`,
          });
        }
      }
    });
  }
  return { rows, compared };
}

function selfTest() {
  const surface = (name, ids) => ({
    surface: name,
    generated_from: "self-test",
    ...(name === "rust" ? { renames: {} } : {}),
    items: [...ids].sort().map((id) => ({ id, kind: "method", owner: null })),
  });
  const p = (name, classes, optional = false) => ({ name, classes, optional });
  const params = {
    rust: {
      "Canvas::save": {
        params: [p("path", ["path"]), p("options", ["object"])],
      },
      "Canvas::draw": { params: [p("x", ["number"]), p("y", ["number"])] },
      "Canvas::draw_sized": {
        params: [
          p("x", ["number"]),
          p("y", ["number"]),
          p("width", ["number"]),
          p("height", ["number"]),
        ],
      },
      "Canvas::load": { params: [p("bytes", ["bytes"])] },
      "Canvas::layer": { params: [] },
      "Canvas::layer_with": { params: [p("alpha", ["number"])] },
      "Canvas::fill": { params: [p("rule", ["enum"])] },
      "Canvas::hit": { params: [p("x", ["number"]), p("y", ["number"])] },
      "Canvas::hit_path": {
        params: [p("path", ["object"]), p("x", ["number"]), p("y", ["number"])],
      },
      "Canvas::text": {
        params: [p("text", ["string"]), p("max_width", ["number"])],
      },
      "Canvas::rects": {
        params: [
          p("range", ["range"]),
          p("height_style", ["enum"]),
          p("width_style", ["enum"]),
        ],
      },
      "Canvas::table": { params: [p("table", ["bytes", "array"])] },
    },
    npm: {
      "Canvas.save": {
        overloads: [
          {
            params: [
              p("filename", ["string", "path"]),
              p("options", ["object"], true),
            ],
          },
        ],
      },
      "Canvas.draw": {
        overloads: [
          {
            params: [
              p("dx", ["number"]),
              p("dy", ["number"]),
              p("dw", ["number"]),
              p("dh", ["number"]),
            ],
          },
        ],
      },
      "Canvas.load": {
        overloads: [{ params: [p("src", ["string", "path", "bytes"])] }],
      },
      "Canvas.layer": {
        overloads: [{ params: [p("alpha", ["number"], true)] }],
      },
      "Canvas.fill": {
        overloads: [
          {
            params: [
              p("fillRule", ["enum"], true),
              p("extra", ["boolean"], true),
            ],
          },
        ],
      },
      "Canvas.hitTest": {
        overloads: [
          {
            params: [
              p("path", ["object"]),
              p("x", ["number"]),
              p("y", ["number"]),
            ],
          },
        ],
      },
      "Canvas.rects": {
        overloads: [
          {
            params: [
              p("start", ["number"]),
              p("end", ["number"]),
              p("hStyle", ["number"], true),
              p("wStyle", ["number"], true),
            ],
          },
        ],
      },
      "Canvas.table": {
        overloads: [{ params: [p("table", ["bytes", "array"])] }],
      },
      "Canvas.text": {
        overloads: [
          {
            params: [
              p("text", ["string"]),
              p("x", ["number"]),
              p("y", ["number"]),
              p("maxWidth", ["number"], true),
            ],
          },
        ],
      },
    },
  };
  const rust = surface("rust", Object.keys(params.rust));
  const npm = surface("npm", Object.keys(params.npm));
  const manifest = parseManifest(
    `
[[capability]]
name = "hit testing"
rust = ["Canvas::hit", "Canvas::hit_path"]
npm  = ["Canvas.hitTest"]
why  = "Two Rust names for one overloaded JavaScript method."
`,
    "self-test",
  );
  const rules = JSON.parse(
    readFileSync(new URL("./parity/rules.json", import.meta.url), "utf8"),
  );
  const { rows, compared } = paramReport({
    rust,
    npm,
    manifest,
    rules: { ...rules, owner_aliases: {}, member_aliases: {} },
    params,
  });
  const has = (npmId, param, kind) =>
    rows.some((r) => r.npm === npmId && r.param === param && r.kind === kind);
  const cases = [
    [
      "a string path matched by position, its URL arm covered by a Rust path",
      !rows.some((r) => r.npm === "Canvas.save"),
    ],
    [
      "four positional numbers met by the suffixed form the gate pairs",
      !rows.some((r) => r.npm === "Canvas.draw"),
    ],
    [
      "a URL, a string and bytes against bytes only is narrower",
      has("Canvas.load", "src", "narrower"),
    ],
    [
      "an argument only the wider sibling takes is found there",
      !rows.some((r) => r.npm === "Canvas.layer"),
    ],
    [
      "a JavaScript argument no Rust function takes is missing",
      has("Canvas.fill", "extra", "missing"),
    ],
    [
      "the camel-cased name of a Rust argument matches it",
      !has("Canvas.fill", "fillRule", "missing"),
    ],
    [
      "two numbers meet one Range, and the arguments after it keep their places",
      compared.has("Canvas.rects") &&
        !rows.some((r) => r.npm === "Canvas.rects"),
    ],
    [
      "a Uint8Array or a number array meets a table of u8",
      compared.has("Canvas.table") &&
        !rows.some((r) => r.npm === "Canvas.table"),
    ],
    [
      "a name match keeps its slot from an earlier positional claim",
      has("Canvas.text", "x", "missing") &&
        !has("Canvas.text", "maxWidth", "missing"),
    ],
    [
      "an entry's Rust function serves the method it names",
      compared.has("Canvas.hitTest") &&
        !rows.some((r) => r.npm === "Canvas.hitTest"),
    ],
  ];
  let bad = 0;
  for (const [label, ok] of cases) {
    if (!ok) {
      console.error(`  self-test FAILED: ${label}`);
      bad += 1;
    }
  }
  if (bad > 0) {
    console.error(JSON.stringify(rows, null, 1));
    process.exit(1);
  }
  console.log(`parity params: self-test, ${cases.length} cases`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  if (process.argv.includes("--self-test")) {
    selfTest();
  } else {
    const read = (p) => JSON.parse(readFileSync(p, "utf8"));
    const rules = JSON.parse(
      readFileSync(new URL("./parity/rules.json", import.meta.url), "utf8"),
    );
    const { rows, compared } = paramReport({
      rust: read("target/parity-rust.json"),
      npm: read("target/parity-npm.json"),
      manifest: parseManifest(
        readFileSync("parity.toml", "utf8"),
        "parity.toml",
      ),
      rules,
      params: read("target/parity-params.json"),
    });
    // `--summary` is the headline alone, for `check-parity`, where rows
    // nobody is yet asked to act on would bury the gate's own output.
    for (const r of process.argv.includes("--summary") ? [] : rows) {
      console.log(
        `${r.kind.padEnd(9)} ${r.npm}#${r.overload} ${r.param}  -> ${r.rust}  [${r.detail}]`,
      );
    }
    const methods = new Set(rows.map((r) => r.npm)).size;
    console.log(
      `parity params (report only): ${rows.length} rows across ${methods} of ${compared.size} compared methods`,
    );
  }
}
