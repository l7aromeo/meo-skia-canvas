// What a caller writes for a paragraph's options, checked by
// `bun run typecheck` against the declarations as the package exports them.
//
// Nothing here runs. A line that stops compiling is the failure.

import type { ParagraphStyleInput, TextHeightBehavior } from "meo-skia-canvas";

type Exactly<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
    ? true
    : false;

// The four codes the binding accepts, and nothing wider: the union is what
// an editor offers.
export const codes: Exactly<TextHeightBehavior, 0 | 1 | 2 | 3> = true;

// Each code is an option value, and nothing else is: a `number` the
// compiler cannot narrow is refused, as the binding refuses one outside the
// four at run time.
const computed: number = 2;
export const options: ParagraphStyleInput[] = [
  { textHeightBehavior: 0 },
  { textHeightBehavior: 3 },
  // @ts-expect-error -- a `number` is wider than the four codes.
  { textHeightBehavior: computed },
];
