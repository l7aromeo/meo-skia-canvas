//
// The public surface of the crate, as rustdoc sees it.
//
// Factored out of `check-public-api.mjs` when a second consumer appeared, so
// that "what is publicly reachable" is answered once. Two tools disagreeing
// about that is the same defect this repository has hit in string tables and
// enum arms: a pair that has to stay symmetrical, with nothing enforcing it.
//
// The reasoning below is that script's, kept with the code it explains. What
// it establishes, briefly: rustdoc's `paths` map over-reports because it
// contains crate-internal items, and grepping the HTML under-reports because
// an undocumented dependency renders without its crate name. Reachability by
// walking the module tree down from the crate root is neither.
//

export const surfaceOf = (doc) => {
  const { index, paths } = doc;
  const item = (id) => index[String(id)];

  // Children an item can contribute to the public surface. Fields and variants
  // count because a `pub` field is as much a signature as a return type, and
  // `impls` count because that is where methods live -- without them the walk
  // reaches no method at all, which was a hole wide enough to pass the four
  // `gui` methods that leaked while the tree reported clean.
  //
  // A variant is a node in its own right, and its payload hangs below it: a
  // struct variant keeps field ids at `kind.struct.fields`, a tuple variant a
  // (nullable) id list at `kind.tuple`. Reaching the variant and stopping there
  // checks nothing, because the ids that name types are one level further down
  // -- `UiEvent::Mouse` alone yields six fields the walk never saw. Tuple
  // structs hide their fields the same way, under `kind.tuple` rather than
  // `kind.plain.fields`; `FontAxisTag` and `ColorMatrix` are the two currently
  // public, `ThreadBound` being the third and reachable from nowhere because
  // `gpu` is `pub(crate)`.
  //
  // Both were invisible until a check of the checker: a `skia_safe::Matrix`
  // planted in `UiEvent::Mouse.point` still reported the tree clean.
  const fieldsOfVariantKind = (kind) => [
    ...(kind?.struct?.fields ?? []),
    // Tuple entries are null where the field is not public, so the nulls are
    // dropped rather than looked up.
    ...(kind?.tuple ?? []).filter((id) => id !== null),
  ];

  const childrenOf = (node) => {
    const inner = node?.inner;
    if (!inner || typeof inner !== "object") return [];
    const [kind] = Object.keys(inner);
    const body = inner[kind];
    switch (kind) {
      case "module":
        return body.items ?? [];
      case "struct":
        return [
          ...(body.kind?.plain?.fields ?? []),
          ...(body.kind?.tuple ?? []).filter((id) => id !== null),
          ...(body.items ?? []),
          ...(body.impls ?? []),
        ];
      case "enum":
        return [
          ...(body.variants ?? []),
          ...(body.items ?? []),
          ...(body.impls ?? []),
        ];
      case "variant":
        return fieldsOfVariantKind(body.kind);
      case "union":
        return [...(body.fields ?? []), ...(body.impls ?? [])];
      case "trait":
        return body.items ?? [];
      case "impl":
        return body.items ?? [];
      default:
        return [];
    }
  };

  // Reachability from the crate root, which is what "public API" means. A
  // `pub(crate)` module is not in the index, so nothing under it is ever walked.
  const owner = new Map();
  const reachable = new Set([doc.root]);
  const queue = [doc.root];

  while (queue.length) {
    const id = queue.shift();
    for (const child of childrenOf(item(id))) {
      if (reachable.has(child)) continue;
      reachable.add(child);
      owner.set(child, id);
      queue.push(child);
    }
  }

  // Prefer rustdoc's own path; fall back to the walk for fields and variants,
  // which have no `paths` entry of their own.
  const nameOf = (id) => {
    const viaPaths = paths[String(id)]?.path;
    if (viaPaths) return viaPaths.join("::");
    const parent = owner.get(id);
    // An impl block has no name and no path of its own; a method under it reads
    // as `Type::method`, so the impl contributes nothing but its parent.
    const self = item(id)?.name;
    if (parent === undefined) return self ?? `<${id}>`;
    return self ? `${nameOf(parent)}::${self}` : nameOf(parent);
  };

  return { item, reachable, owner, nameOf, index, paths };
};
