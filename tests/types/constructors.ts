// What a caller writes against the exported constructor objects, checked by
// `bun run typecheck` as a consumer would see them: through the package's own
// name, so the `exports` map picks the declarations.
//
// Nothing here runs. A line that stops compiling is the failure.

import {
  CanvasGradient,
  CanvasRenderingContext2D,
  DOMMatrix,
  DOMPoint,
  DOMRect,
  Path2D,
  TextMetrics,
} from "meo-skia-canvas";

// `instanceof` narrows to the instance type, on the constructors that can be
// called and on the three that throw when called and so declare no
// construct signature.
export function narrows(value: unknown): void {
  if (value instanceof CanvasGradient) value satisfies CanvasGradient;
  if (value instanceof CanvasRenderingContext2D)
    value satisfies CanvasRenderingContext2D;
  if (value instanceof TextMetrics) value satisfies TextMetrics;
  if (value instanceof DOMMatrix) value satisfies DOMMatrix;
  if (value instanceof DOMPoint) value satisfies DOMPoint;
  if (value instanceof DOMRect) value satisfies DOMRect;
  if (value instanceof Path2D) value satisfies Path2D;
}

// `prototype` is the instance type, not `any`: these are `declare var`s, and
// TypeScript supplies a `prototype` only for a class, so each is declared.
// Without the declaration, one with a construct signature falls back to
// `Function.prototype`, which is `any` and assignable to anything, so the
// check is that the type is exactly the instance type.
type Exactly<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
    ? true
    : false;

export const prototypes: [
  Exactly<typeof CanvasGradient.prototype, CanvasGradient>,
  Exactly<typeof CanvasRenderingContext2D.prototype, CanvasRenderingContext2D>,
  Exactly<typeof TextMetrics.prototype, TextMetrics>,
  Exactly<typeof DOMMatrix.prototype, DOMMatrix>,
  Exactly<typeof DOMPoint.prototype, DOMPoint>,
  Exactly<typeof DOMRect.prototype, DOMRect>,
  Exactly<typeof Path2D.prototype, Path2D>,
] = [true, true, true, true, true, true, true];
