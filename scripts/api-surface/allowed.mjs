//
// Members reachable at runtime that `lib/index.d.ts` deliberately does not
// declare.
//
// Every entry carries the reason it is exempt and what would retire it. This
// is not a suppression list: an entry naming something that is no longer
// reachable fails the check, the same way an undeclared member does. A list
// that only ever grows stops describing the code and starts hiding it.
//
export const ALLOWED = [
  {
    member: "CanvasRenderingContext2D.textTracking",
    why:
      "A deprecation shim. The setter exists only to throw a message naming " +
      "`letterSpacing` as the replacement, so JavaScript that still assigns " +
      "it is told what to do. Declaring it would offer it to TypeScript, " +
      "which is the opposite of the intent. Retire this when the shim goes.",
  },
  {
    member: "App.native",
    why: "The wrapper's handle onto the Neon binding, on the exported instance.",
  },
  {
    member: "FontLibrary.native",
    why: "As `App.native` -- the binding handle on an exported instance.",
  },
  {
    member: "CanvasRenderingContext2D.records",
    why:
      "Set by `drawlist.guard` to mark a class whose drawing is recorded. " +
      "Internal dispatch state, not API.",
  },
  {
    member: "Path2D.records",
    why: "As `CanvasRenderingContext2D.records` -- set by the same `guard`.",
  },
];
