---
id: api-intro
---

# API Documentation

:::info[Note]
Documentation for the key classes and their attributes are listed below—properties are printed in **bold** and methods have parentheses attached to the name. The instances where Skia Canvas’s behavior goes beyond the standard are marked by a 🧪 symbol, linking to further details below. Links to documentation for the web standards Skia Canvas emulates are marked with a 📖.
:::

The library exports a number of classes emulating familiar browser objects including:

- [Canvas][mdn_canvas] ⧸ [extensions][canvas] 🧪
- [CanvasGradient][CanvasGradient]
- [CanvasPattern][CanvasPattern]
- [CanvasRenderingContext2D][CanvasRenderingContext2D] ⧸ [extensions][context] 🧪
- [DOMMatrix][DOMMatrix]
- [Image][Image] / [extensions][image] 🧪
- [ImageData][ImageData] / [extensions][imagedata] 🧪
- [Path2D][p2d_mdn] ⧸ [extensions][path2d] 🧪

In addition, the module contains:

- [FontLibrary][fontlibrary] a global object for inspecting the system’s fonts and loading additional ones
- [Window][window] a class allowing you to display your canvas interactively in an on-screen window
- [App][app] a helper class for coordinating multiple windows in a single script
- [loadImage()][loadimage] a utility function for loading `Image` objects asynchronously
- [loadImageData()][loadimagedata] a utility function for loading `ImageData` objects asynchronously

The module also exports a set of Skia effect and typography classes that have no browser counterpart at all. The filters are assigned to a context and apply to everything drawn afterward; the typography classes replace `fillText()` when you need wrapping or more than one style:

- [ColorFilter][colorfilter] remaps each pixel's color as it is drawn — matrices, lookup tables, blends 🧪
- [ImageFilter][imagefilter] composable pixel effects — blur, drop shadow, warp, convolution, lighting 🧪
- [MaskFilter][maskfilter] styled coverage blurs for glows, halos, and inner shadows 🧪
- [Shader][shader] procedural noise usable in place of a `fillStyle` or `strokeStyle` color 🧪
- [ParagraphBuilder][paragraphbuilder] assembles runs of styled text into a `Paragraph` 🧪
- [Paragraph][paragraph] a shaped block of text you can wrap, measure, hit-test, and draw 🧪

The same library is also a Rust crate, with a Canvas-shaped facade of its own:

- [Rust consumer API][native-rust] `Canvas`, `Context2D`, `PathBuilder`, surfaces, and the typed error set

---

For detailed notes on the extensions Skia Canvas has made to standard object types, see the corresponding pages:

import DocCardList from '@theme/DocCardList';

<DocCardList />

<!-- references_begin -->

[app]: app.md
[native-rust]: native-rust.md
[canvas]: canvas.md
[colorfilter]: color-filter.md
[context]: context.md
[fontlibrary]: font-library.md
[imagefilter]: image-filter.md
[maskfilter]: mask-filter.md
[paragraph]: paragraph.md
[paragraphbuilder]: paragraph-builder.md
[shader]: shader.md
[loadimage]: image.md#loadimage
[image]: image.md
[imagedata]: imagedata.md
[loadimagedata]: imagedata.md#loadimagedata
[path2d]: path2d.md
[window]: window.md
[p2d_mdn]: https://developer.mozilla.org/en-US/docs/Web/API/Path2D
[mdn_canvas]: https://developer.mozilla.org/en-US/docs/Web/API/Canvas
[CanvasGradient]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasGradient
[CanvasPattern]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasPattern
[CanvasRenderingContext2D]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D
[DOMMatrix]: https://developer.mozilla.org/en-US/docs/Web/API/DOMMatrix
[Image]: https://developer.mozilla.org/en-US/docs/Web/API/Image
[ImageData]: https://developer.mozilla.org/en-US/docs/Web/API/ImageData

<!-- references_end -->
