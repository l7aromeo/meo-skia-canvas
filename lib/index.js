//
// Skia Canvas — CommonJS version
//

"use strict";

const {
    Canvas,
    CanvasGradient,
    CanvasPattern,
    CanvasTexture,
  } = require("./classes/canvas"),
  {
    ColorFilter,
    ImageFilter,
    MaskFilter,
    Shader,
  } = require("./classes/filter"),
  { ColorMatrix } = require("./classes/color_matrix"),
  { Image, ImageData, loadImage, loadImageData } = require("./classes/imagery"),
  { DOMPoint, DOMMatrix, DOMRect } = require("./classes/geometry"),
  {
    TextMetrics,
    FontLibrary,
    PlaceholderAlignment,
    TextBaseline,
    TextDecoration,
    TextDecorationStyle,
    ParagraphBuilder,
    Paragraph,
  } = require("./classes/typography"),
  { CanvasRenderingContext2D } = require("./classes/context"),
  { App, Window } = require("./classes/gui"),
  { Path2D } = require("./classes/path"),
  { skiaNode } = require("./classes/neon");

/**
 * Get backend information without creating a canvas.
 * Returns { renderer: "CPU"|"GPU", api: string|null, device: string, driver?: string, threads: number, gpuAvailable: boolean, error?: string }
 */
function backend() {
  return JSON.parse(skiaNode.backend());
}

module.exports = {
  Canvas,
  CanvasGradient,
  CanvasPattern,
  CanvasTexture,
  ColorFilter,
  ImageFilter,
  MaskFilter,
  Shader,
  ColorMatrix,
  Image,
  ImageData,
  loadImage,
  loadImageData,
  Path2D,
  DOMPoint,
  DOMMatrix,
  DOMRect,
  FontLibrary,
  TextMetrics,
  PlaceholderAlignment,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
  ParagraphBuilder,
  Paragraph,
  CanvasRenderingContext2D,
  App,
  Window,
  backend,
};
