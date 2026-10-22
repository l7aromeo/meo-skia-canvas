#!/usr/bin/env node
//
// The parameter pass of the parity gate.
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
// `draw_image_region`; the Rust functions of a two-sided entry naming the
// method; and those `parameter_links` in `rules.json` names. Each overload
// is held to whichever candidate covers most of it, as `fit` describes, and
// a union several equally good candidates serve between them is served. A
// method with no candidate is the item gate's business, not this pass's.
//
// THE REGISTER is `parity.toml`. An entry's `npm` list may hold parameter
// ids, `Holder.method(param)` or `Holder.method(*)`, and a row such an id
// names is recorded under that entry's status and reason. A row nothing
// records fails, and so does an id naming no real argument or recording no
// row, so the register cannot outlive what it describes.
//
// `--report-only` prints the same and exits 0; `check-parity` runs it that
// way until the register's first triage is agreed. `--all` lists recorded
// rows too.
//
// Usage: node scripts/check-parity-params.mjs [--self-test | --all]
//   [--report-only], reading target/parity-rust.json,
//   target/parity-npm.json, target/parity-params.json and parity.toml;
//   `just report-parity-params` writes all four and prints every row.

import { readFileSync } from "node:fs";
import { PARAMETER_ID, nameMaps } from "./check-parity.mjs";
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
 * struct takes a list, because JavaScript spells a point, a rectangle or a
 * size as a tuple where the crate names a struct -- the coarsest of these,
 * and the one most able to hide a real difference.
 *
 * Not the other way: a Rust list does not take a JavaScript object. That
 * direction let `use(families: {name: paths})` pass against
 * `use_paths(alias, paths)`, a map against a list of paths, and let an
 * options object stand in for an unrelated array field.
 */
const ACCEPTS = {
  string: ["string", "enum", "path"],
  enum: ["enum", "string", "number"],
  path: ["path", "string"],
  number: ["number"],
  boolean: ["boolean"],
  bytes: ["bytes"],
  array: ["array"],
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
 * Whether JavaScript parameter `p` names `name`: in snake case, as written,
 * or through `parameter_aliases` in `rules.json`, which records a synonym
 * the two surfaces spell differently -- `inverted` for `invert`.
 */
const names = (p, name, aliases) =>
  name === snake(p.name) ||
  name === p.name ||
  (aliases[p.name] ?? []).includes(name);

/**
 * How one JavaScript overload maps onto one Rust function.
 *
 * A static JavaScript helper's first argument is the value a Rust method
 * takes as `self`. Then every name match, against a Rust parameter or a
 * field of a Rust struct parameter -- `toDataURL(type, quality)` reaches the
 * `quality` field of `EncodeOptions`, and `originX` reaches `x` of a
 * parameter named `origin`. Then the rest by position, as a cursor
 * following the last slot used: a `Range` takes two numbers, and a struct
 * whose fields are all public takes consecutive arguments field by field
 * when the argument is not the struct itself -- six numbers for an
 * `Affine`, `width, height, align, baseline, offset` for a `Placeholder`.
 * One pass doing names before positions lets a name keep its slot from an
 * earlier positional claim: `x` claiming `max_width` before `maxWidth` is
 * reached.
 */
function fit(overload, fn, aliases) {
  const slot = new Array(overload.params.length).fill(null);
  const whole = new Set();
  const parts = new Map();
  let start = 0;
  if (
    overload.static &&
    fn.receiver &&
    overload.params.length > 0 &&
    covered(overload.params[0].classes, ["object"]).length > 0
  ) {
    slot[0] = { receiver: true };
    start = 1;
  }
  overload.params.forEach((p, at) => {
    if (at < start) return;
    // A struct some fields of which were taken by name can still be named
    // whole by an object, which then carries the rest of it:
    // `createTexture(spacing, options)` passes `TextureOptions.spacing` on
    // its own and every other field in `options`.
    const k = fn.params.findIndex(
      (r, i) =>
        !whole.has(i) &&
        (!parts.has(i) || p.classes.includes("object")) &&
        names(p, r.name, aliases) &&
        covered(p.classes, r.classes).length > 0,
    );
    if (k !== -1) {
      slot[at] = { k };
      whole.add(k);
      return;
    }
    for (const [i, r] of fn.params.entries()) {
      if (whole.has(i)) continue;
      const j = (r.fields ?? []).findIndex(
        (f, n) =>
          !parts.get(i)?.has(n) &&
          (names(p, f.name, aliases) ||
            snake(p.name) === `${r.name}_${f.name}`) &&
          covered(p.classes, f.classes).length > 0,
      );
      if (j !== -1) {
        slot[at] = { k: i, field: j };
        parts.set(i, (parts.get(i) ?? new Set()).add(j));
        return;
      }
    }
  });
  let previous = -1;
  let spread = null;
  overload.params.forEach((p, at) => {
    if (at < start) return;
    // A field matched by name leaves the cursor inside its struct, so the
    // next argument takes the next field still free: `a, b, c, d` meet an
    // `Affine`'s fields by name and `e, f` then take `tx` and `ty`.
    if (slot[at]) {
      previous = slot[at].k;
      spread =
        slot[at].field === undefined
          ? null
          : { k: slot[at].k, next: slot[at].field + 1 };
      return;
    }
    if (
      previous !== -1 &&
      fn.params[previous].classes.includes("range") &&
      p.classes.includes("number") &&
      slot.filter((s) => s?.k === previous).length === 1
    ) {
      slot[at] = { k: previous };
      return;
    }
    if (spread) {
      const taken = parts.get(spread.k) ?? new Set();
      while (taken.has(spread.next)) spread.next += 1;
      const f = fn.params[spread.k].fields?.[spread.next];
      if (f && covered(p.classes, f.classes).length > 0) {
        slot[at] = { k: spread.k, field: spread.next };
        parts.set(spread.k, taken.add(spread.next));
        spread.next += 1;
        return;
      }
      spread = null;
    }
    // The next free Rust slot, passing over an optional one the argument
    // does not fit: JavaScript omits an argument where Rust passes `None`,
    // so `use(fontPaths)` reaches `use_paths(alias, paths)` at `paths`. A
    // required slot it does not fit stops the search.
    for (let next = previous + 1; next < fn.params.length; next += 1) {
      if (whole.has(next) || parts.has(next)) continue;
      const r = fn.params[next];
      if (covered(p.classes, r.classes).length > 0) {
        slot[at] = { k: next };
        whole.add(next);
        previous = next;
        return;
      }
      if (r.fields && covered(p.classes, r.fields[0].classes).length > 0) {
        slot[at] = { k: next, field: 0 };
        whole.add(next);
        spread = { k: next, next: 1 };
        previous = next;
        return;
      }
      if (!r.optional) return;
    }
  });
  const outcome = overload.params.map((p, at) => {
    const s = slot[at];
    if (!s) return { param: p, rust: null, kept: [] };
    const target = s.receiver
      ? { name: "self", classes: ["object"] }
      : s.field === undefined
        ? fn.params[s.k]
        : {
            ...fn.params[s.k].fields[s.field],
            name: `${fn.params[s.k].name}.${fn.params[s.k].fields[s.field].name}`,
          };
    return { param: p, rust: target, kept: covered(p.classes, target.classes) };
  });
  return { score: outcome.filter((o) => o.rust).length, outcome };
}

/**
 * Every row the pass reports, as `{ npm, overload, param, kind, rust,
 * detail, recorded }`, with `recorded` the name of the entry that records
 * it or `null`; the methods compared; the problems with the parameter ids
 * and links themselves, which fail whatever the rows say; and, per overload,
 * the candidates it was held to with the slot each argument took.
 */
export function paramReport({ rust, npm, manifest, rules, params }) {
  const { rustNames, npmNames } = nameMaps({ rust, npm, rules });
  const aliases = rules.parameter_aliases ?? {};
  const links = rules.parameter_links ?? {};
  const problems = [];
  const byName = new Map();
  for (const [id, names] of rustNames) {
    if (!params.rust[id]) continue;
    for (const n of names) byName.set(n, [...(byName.get(n) ?? []), id]);
  }
  for (const [id, targets] of Object.entries(links)) {
    if (id.startsWith("_")) continue;
    if (!params.npm[id])
      problems.push(
        `parameter_links names \`${id}\`, which declares no parameters`,
      );
    for (const r of targets)
      if (!params.rust[r])
        problems.push(
          `parameter_links sends \`${id}\` to \`${r}\`, which is not a Rust function`,
        );
  }
  const rustFns = Object.keys(params.rust);
  const rows = [];
  // Which candidates each overload was held to and where each argument
  // landed in them, for a change to the pass to be measured by more than
  // the rows it moves: an argument that newly finds a slot moves none.
  const traces = [];
  const compared = new Set();
  for (const [id, record] of Object.entries(params.npm)) {
    const paired = new Set();
    for (const n of npmNames.get(id) ?? [])
      for (const r of byName.get(n) ?? []) paired.add(r);
    // An entry naming the method serves it with its Rust functions. Not
    // narrowed to the method's own holder: `createLinearGradient` is served
    // by `Shader::linear_gradient` across holders, and an entry that groups
    // absences rather than counterparts records its parameters instead.
    for (const entry of manifest) {
      if (!entry.rust.length || !entry.npm.includes(id)) continue;
      for (const r of entry.rust) if (params.rust[r]) paired.add(r);
    }
    for (const r of [...paired]) {
      const prefix = `${r}_`;
      for (const f of rustFns) if (f.startsWith(prefix)) paired.add(f);
    }
    for (const r of links[id] ?? []) if (params.rust[r]) paired.add(r);
    if (paired.size === 0) continue;
    compared.add(id);
    record.overloads.forEach((overload, at) => {
      const fits = [...paired]
        .sort()
        .map((r) => ({ rust: r, ...fit(overload, params.rust[r], aliases) }));
      const best = fits.reduce((a, b) => (b.score > a.score ? b : a));
      // A JavaScript union several Rust functions serve between them is
      // served: `new Path2D(path: string | Path2D)` is `Path2D::from_svg`
      // for the one and `PathBuilder::from_path` for the other. Only the
      // candidates fitting as many arguments as the best one count.
      const peers = fits.filter((f) => f.score === best.score);
      traces.push({
        npm: id,
        overload: at,
        slots: peers.map(
          (f) =>
            `${f.rust}(${f.outcome.map((o) => o.rust?.name ?? "-").join(", ")})`,
        ),
      });
      best.outcome.forEach((o, i) => {
        const kept = new Set(peers.flatMap((f) => f.outcome[i].kept));
        const lost = o.param.classes.filter((c) => !kept.has(c));
        const base = { npm: id, overload: at, param: o.param.name };
        if (!peers.some((f) => f.outcome[i].rust)) {
          rows.push({
            ...base,
            kind: "missing",
            rust: best.rust,
            detail: `${o.param.classes.join(" | ")}${o.param.optional ? ", optional" : ""}`,
          });
        } else if (lost.length > 0) {
          rows.push({
            ...base,
            kind: "narrower",
            rust: `${best.rust}(${o.rust?.name ?? "?"})`,
            detail: `${lost.join(" | ")} not taken; Rust has ${(o.rust?.classes ?? []).join(" | ")}`,
          });
        }
      });
    });
  }
  // The register. A parameter id records every row for that argument of
  // that method, in any overload; `(*)` records every row for the method.
  const registered = new Map();
  for (const entry of manifest) {
    for (const id of entry.npm) {
      const m = PARAMETER_ID.exec(id);
      if (!m) continue;
      const { method, param } = m.groups;
      const record = params.npm[method];
      if (!record) {
        problems.push(
          `\`${id}\` in '${entry.name}': \`${method}\` declares no parameters`,
        );
        continue;
      }
      if (
        param !== "*" &&
        !record.overloads.some((o) => o.params.some((p) => p.name === param))
      ) {
        problems.push(
          `\`${id}\` in '${entry.name}': \`${method}\` has no parameter \`${param}\``,
        );
        continue;
      }
      registered.set(id, { entry: entry.name, used: false });
    }
  }
  for (const r of rows) {
    const hit =
      registered.get(`${r.npm}(${r.param})`) ?? registered.get(`${r.npm}(*)`);
    r.recorded = hit?.entry ?? null;
    if (hit) hit.used = true;
  }
  for (const [id, { entry, used }] of registered) {
    if (!used)
      problems.push(
        `\`${id}\` in '${entry}' records no row: the argument it describes is now served, so the id is stale`,
      );
  }
  return { rows, compared, problems, traces };
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
      "Canvas::encode": {
        params: [
          p("format", ["enum"]),
          {
            ...p("options", ["object"]),
            fields: [p("path", ["object"]), p("quality", ["number"])],
          },
        ],
      },
      "Canvas::set_transform": {
        params: [
          {
            ...p("transform", ["object", "string"]),
            fields: ["a", "b", "c", "d", "tx", "ty"].map((n) =>
              p(n, ["number"]),
            ),
          },
        ],
      },
      "Canvas::place": {
        params: [
          {
            ...p("placeholder", ["object"]),
            fields: [
              p("width", ["number"]),
              p("height", ["number"]),
              p("alignment", ["enum"]),
            ],
          },
        ],
      },
      "Matrix::post_translate": {
        receiver: true,
        params: [p("dx", ["number"]), p("dy", ["number"])],
      },
      "Canvas::trim": {
        params: [
          p("start", ["number"]),
          p("end", ["number"]),
          p("invert", ["boolean"]),
        ],
      },
      "Canvas::probe": { params: [p("x", ["number"])] },
      "Canvas::hit_inside": {
        params: [p("path", ["object"]), p("x", ["number"])],
      },
      "Canvas::open": { params: [p("data", ["string"])] },
      "Canvas::pixels": {
        params: [
          p("width", ["number"]),
          p("height", ["number"]),
          {
            ...p("options", ["object"]),
            fields: [p("color_space", ["enum"])],
          },
          p("pixels", ["bytes", "array"]),
        ],
      },
      "Canvas::use_paths": {
        params: [p("alias", ["string"], true), p("paths", ["array"])],
      },
      "Canvas::use_map": {
        params: [p("alias", ["string"], true), p("paths", ["array"])],
      },
      "Canvas::texture": {
        params: [
          {
            ...p("options", ["object"]),
            fields: [
              p("line", ["number"]),
              p("spacing", ["array"]),
              p("offset", ["array"]),
            ],
          },
        ],
      },
      "Canvas::zoom": {
        params: [
          p("sx", ["number"]),
          p("sy", ["number"]),
          {
            ...p("origin", ["object"]),
            fields: [p("x", ["number"]), p("y", ["number"])],
          },
        ],
      },
      "Canvas::from_path": { params: [p("path", ["object"])] },
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
      "Canvas.encode": {
        overloads: [
          { params: [p("type", ["enum"]), p("quality", ["number"], true)] },
        ],
      },
      "Canvas.setTransform": {
        overloads: [
          {
            params: ["a", "b", "c", "d", "e", "f"].map((n) => p(n, ["number"])),
          },
          { params: [p("transform", ["string"])] },
        ],
      },
      "Canvas.place": {
        overloads: [
          {
            params: [
              p("w", ["number"]),
              p("h", ["number"]),
              p("align", ["number"]),
            ],
          },
        ],
      },
      "Matrix.postTranslate": {
        overloads: [
          {
            static: true,
            params: [
              p("m", ["array"]),
              p("dx", ["number"]),
              p("dy", ["number"]),
            ],
          },
        ],
      },
      "Canvas.trim": {
        overloads: [
          { params: [p("start", ["number"]), p("inverted", ["boolean"])] },
        ],
      },
      "Canvas.probe": {
        overloads: [{ params: [p("path", ["object"]), p("x", ["number"])] }],
      },
      "Canvas.open": {
        overloads: [{ params: [p("path", ["string", "object"])] }],
      },
      "Canvas.pixels": {
        overloads: [
          {
            params: [
              p("src", ["bytes"]),
              p("width", ["number"]),
              p("height", ["number"]),
            ],
          },
        ],
      },
      "Canvas.usePaths": {
        overloads: [{ params: [p("fontPaths", ["array"])] }],
      },
      "Canvas.useMap": {
        overloads: [{ params: [p("families", ["object"])] }],
      },
      "Canvas.texture": {
        overloads: [
          { params: [p("spacing", ["array"]), p("options", ["object"], true)] },
        ],
      },
      "Canvas.zoom": {
        overloads: [
          {
            params: [
              p("scaleX", ["number"], true),
              p("scaleY", ["number"], true),
              p("scaleZ", ["number"], true),
              p("originX", ["number"], true),
              p("originY", ["number"], true),
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

[[capability]]
name = "recorded arguments"
rust = []
npm  = ["Canvas.fill(extra)", "Canvas.load(*)"]
why  = "One argument and one whole method, each recording rows."

[[capability]]
name = "broken records"
rust = []
npm  = ["Canvas.nothing(x)", "Canvas.fill(zzz)", "Canvas.draw(dx)"]
why  = "A method with no parameters, an argument it lacks, and a stale one."
`,
    "self-test",
  );
  const rules = JSON.parse(
    readFileSync(new URL("./parity/rules.json", import.meta.url), "utf8"),
  );
  const testRules = {
    ...rules,
    owner_aliases: {},
    member_aliases: {},
    parameter_aliases: { inverted: ["invert"] },
    parameter_links: {
      "Canvas.probe": ["Canvas::hit_inside"],
      "Canvas.open": ["Canvas::from_path"],
      "Canvas.ghost": ["Canvas::fill"],
      "Canvas.fill": ["Canvas::nowhere"],
    },
  };
  const run = (overrides = {}) =>
    paramReport({
      rust,
      npm,
      manifest,
      rules: { ...testRules, ...overrides },
      params,
    });
  const { rows, compared, problems } = run();
  // The same surfaces without the alias and without the links, so each case
  // below that relies on one can be shown to need it.
  const bare = run({ parameter_aliases: {}, parameter_links: {} }).rows;
  // With the alias table `rules.json` actually ships, rather than this
  // test's own: the case below is about that table's `src` line.
  const shipped = run({ parameter_aliases: rules.parameter_aliases });
  const clean = (npmId) =>
    compared.has(npmId) && !rows.some((r) => r.npm === npmId);
  const problem = (text) => problems.some((q) => q.includes(text));
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
    [
      "a positional argument reaches a field of an options struct by name",
      clean("Canvas.encode"),
    ],
    [
      "six numbers meet a struct's fields, four by name and two after them",
      clean("Canvas.setTransform"),
    ],
    [
      "a struct is spread positionally, an enum field taking a number",
      clean("Canvas.place"),
    ],
    [
      "a static helper's first argument is the Rust method's receiver",
      clean("Matrix.postTranslate"),
    ],
    [
      "an aliased name matches, and is missing without the alias",
      clean("Canvas.trim") &&
        bare.some((r) => r.npm === "Canvas.trim" && r.param === "inverted"),
    ],
    [
      "a linked function serves an argument, which is missing unlinked",
      clean("Canvas.probe") &&
        bare.some((r) => r.npm === "Canvas.probe" && r.param === "path"),
    ],
    [
      "a union two Rust functions serve between them is served",
      clean("Canvas.open") &&
        bare.some((r) => r.npm === "Canvas.open" && r.kind === "narrower"),
    ],
    [
      "originX and originY reach an origin's fields by name, leaving scaleZ out",
      compared.has("Canvas.zoom") &&
        rows
          .filter((r) => r.npm === "Canvas.zoom")
          .map((r) => r.param)
          .join() === "scaleZ",
    ],
    [
      "an argument passes over an optional Rust slot it does not fit",
      clean("Canvas.usePaths"),
    ],
    [
      "a JavaScript object does not stand in for a Rust list",
      rows.some((r) => r.npm === "Canvas.useMap" && r.param === "families"),
    ],
    [
      "an object names the rest of a struct whose field went by name",
      clean("Canvas.texture"),
    ],
    [
      "`src` reaches a `pixels` no position reaches, through the shipped alias",
      shipped.compared.has("Canvas.pixels") &&
        !shipped.rows.some((r) => r.npm === "Canvas.pixels") &&
        bare.some((r) => r.npm === "Canvas.pixels" && r.param === "src"),
    ],
    [
      "a parameter id records the row for that argument",
      rows.find((r) => r.npm === "Canvas.fill" && r.param === "extra")
        ?.recorded === "recorded arguments",
    ],
    [
      "a (*) id records every row of the method",
      rows
        .filter((r) => r.npm === "Canvas.load")
        .every((r) => r.recorded === "recorded arguments") &&
        rows.some((r) => r.npm === "Canvas.load"),
    ],
    [
      "a row no id names stays unrecorded",
      rows.find((r) => r.npm === "Canvas.text" && r.param === "x")?.recorded ===
        null,
    ],
    [
      "an id for a method with no parameters is refused",
      problem(
        "`Canvas.nothing(x)` in 'broken records': `Canvas.nothing` declares no parameters",
      ),
    ],
    [
      "an id for an argument the method lacks is refused",
      problem(
        "`Canvas.fill(zzz)` in 'broken records': `Canvas.fill` has no parameter `zzz`",
      ),
    ],
    [
      "an id recording no row is refused as stale",
      problem("`Canvas.draw(dx)` in 'broken records' records no row"),
    ],
    [
      "a link from a method without parameters is refused",
      problem("`Canvas.ghost`"),
    ],
    [
      "a link to a Rust function that does not exist is refused",
      problem("`Canvas::nowhere`"),
    ],
    ["nothing else is refused", problems.length === 5],
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
    const { rows, compared, problems } = paramReport({
      rust: read("target/parity-rust.json"),
      npm: read("target/parity-npm.json"),
      manifest: parseManifest(
        readFileSync("parity.toml", "utf8"),
        "parity.toml",
      ),
      rules,
      params: read("target/parity-params.json"),
    });
    // `--all` lists every row, recorded or not, for triage. Otherwise the
    // output is what fails: rows nothing records, and problems with the
    // register itself. `--report-only` prints the same and exits 0.
    const all = process.argv.includes("--all");
    const open = rows.filter((r) => r.recorded === null);
    for (const r of all ? rows : open) {
      console.log(
        `${r.kind.padEnd(9)} ${r.npm}#${r.overload} ${r.param}  -> ${r.rust}  [${r.detail}]${r.recorded ? `  recorded: ${r.recorded}` : ""}`,
      );
    }
    for (const p of problems) console.error(`  ${p}`);
    const methods = new Set(rows.map((r) => r.npm)).size;
    const failing = open.length > 0 || problems.length > 0;
    const reportOnly = process.argv.includes("--report-only");
    console.log(
      `parity params${reportOnly ? " (report only)" : ""}: ${rows.length} rows across ${methods} of ${compared.size} compared methods, ` +
        `${rows.length - open.length} recorded, ${open.length} not, ${problems.length} register problems`,
    );
    if (failing && !reportOnly) process.exit(1);
  }
}
