//
// Types for the browser build (lib/browser.js).
//
// A re-export list rather than a second set of declarations: the shapes stay in
// index.d.ts and cannot drift, and only the membership of this list has to be
// maintained. It must match `module.exports` in browser.js.
//
// The Node build exports considerably more. App, Window, FontLibrary,
// CanvasTexture, ColorFilter, ImageFilter, MaskFilter, Shader, Paragraph,
// ParagraphBuilder and backend are all backed by Skia, the filesystem or a GUI
// event loop, so they are absent here rather than declared and undefined.
//

export {
  Canvas,
  CanvasGradient,
  CanvasPattern,
  CanvasRenderingContext2D,
  ColorMatrix,
  DOMMatrix,
  DOMPoint,
  DOMRect,
  Image,
  ImageData,
  Path2D,
  PlaceholderAlignment,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
  loadImage,
  loadImageData,
} from "./index";

// Types carry no runtime weight, so the browser build can use the same ones.
export type {
  Color4fInput,
  ColorSpace,
  ColorType,
  ExportFormat,
  ExportOptions,
  Path2DBounds,
  Path2DEdge,
  RenderOptions,
  SaveOptions,
  TextMetricsLine,
  TextMetricsRun,
} from "./index";
