# The declared surface against the real one

`lib/index.d.ts` is a claim about what the addon exposes. Nothing else in this
repository checks that the claim is true. `tsc` believes the declarations, the
test suite exercises the runtime, and each half is internally consistent — so
a member the runtime grew and nobody declared, or a member declared and never
built, passes every gate there is.

Thirteen defects of that shape were found the first time the two were compared
by hand, and every one of them had been shipping.

## Both directions fail

A member reachable at runtime and undeclared is invisible to every TypeScript
caller: `locale` and `strokeWidth` were read by the paragraph parser and
declared nowhere, so writing either was a type error against a key that works.
A member declared and absent is the reverse promise — `ImageData.prototype`
was declared and not there.

## Three things this has to get right, all of which it got wrong first

**Mixin heritage.** The declarations follow WebIDL and split the context
across `CanvasPath`, `CanvasDrawPath` and a dozen more interfaces that
`CanvasRenderingContext2D` and `Path2D` extend, so a member is usually
declared on an interface whose name no caller writes. Closing over `extends`
took the false count from 147 to 86.

**Bases from other packages.** `Image` and `Window` extend `EventEmitter` from
`"stream"`. Without loading `@types/node`'s `events.d.ts`, 29 emitter methods
report as undeclared against declarations that compile clean. That file lives
in the _root_ `node_modules`, which is why the recipe depends on `ensure-deps`
as well as on this directory's own install.

**Instances, not just prototypes.** `App` and `FontLibrary` are exported as
objects rather than classes, so a walk written against `.prototype` misses
both. `TextMetrics` is the opposite case: its declaration describes a
measurement, not the bare constructor the module exports, so it is compared
against a real `measureText` result or its fourteen members all read as
missing.

## Its own TypeScript

The root package is on TypeScript 7, whose entry point exports `version` and
`versionMajorMinor` and no compiler API at all. `scripts/typedoc` pins its own
copy for the same reason; this is the second instance rather than a new idea.

## The allowlist is not a suppression list

`allowed.mjs` carries the reason each entry is exempt and what would retire
it. An entry naming something no longer reachable **fails the check**, the
same way an undeclared member does — a list that only grows stops describing
the code and starts hiding it. That is the shape the browser-build guard in
`tests/static/binary.test.js` already uses, which fails both ways too.
