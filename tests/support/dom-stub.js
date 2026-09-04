//
// Just enough of a browser for `lib/browser.js` to load and run.
//
// That file is a parallel implementation of `exportOptions`, and it is
// unexercised: it destructures `window` at module scope and reaches for
// `document.createElement` in its constructor, so requiring it under node
// throws before any of its logic runs. Nothing under `tests/` loaded it, which
// is why two fixes to `lib/classes/canvas.js` never reached their twin here.
//
// This is deliberately not a DOM emulator. It implements the handful of calls
// that file actually makes -- a canvas element with `getContext`, `toBlob` and
// `toDataURL`, an anchor that records a download instead of performing one,
// and `URL.createObjectURL` -- so the export-option logic runs for real
// against fakes that record what they were asked for.
//

"use strict";

/** A 2D context that records the calls `browser.js` makes on it. */
function fakeContext(canvas) {
  return {
    canvas,
    fillStyle: "",
    calls: [],
    fillRect(...args) {
      this.calls.push(["fillRect", ...args]);
    },
    drawImage(...args) {
      this.calls.push(["drawImage", args[0], ...args.slice(1)]);
    },
    scale(...args) {
      this.calls.push(["scale", ...args]);
    },
  };
}

/**
 * A stand-in for a `<canvas>` element.
 *
 * `toBlob` produces a Blob whose bytes name the format and size asked for, so
 * a test can tell one export from another without encoding anything.
 */
function fakeCanvas() {
  const element = {
    nodeName: "CANVAS",
    width: 0,
    height: 0,
    style: {},
    getContext() {
      return (element._ctx ||= fakeContext(element));
    },
    toBlob(callback, mime, quality) {
      element.encoded = {
        mime,
        quality,
        width: element.width,
        height: element.height,
      };
      callback(
        new Blob([`${mime}:${element.width}x${element.height}:${quality}`], {
          type: mime,
        }),
      );
    },
    toDataURL(mime, quality) {
      element.encoded = {
        mime,
        quality,
        width: element.width,
        height: element.height,
      };
      return `data:${mime};base64,${Buffer.from(
        `${element.width}x${element.height}:${quality}`,
      ).toString("base64")}`;
    },
  };
  return element;
}

/** Downloads `_download` was asked to perform, most recent last. */
const downloads = [];

/**
 * Installs the globals `lib/browser.js` reads, and returns a teardown.
 *
 * Must run before the first `require` of that file: it destructures `window`
 * at module scope, so the globals have to exist by then.
 */
function installDOM() {
  const anchors = [];
  const body = {
    appendChild: (node) => anchors.push(node),
    removeChild: () => {},
  };

  const document = {
    body,
    createElement(tag) {
      if (tag === "canvas") return fakeCanvas();
      if (tag === "a") {
        return {
          nodeName: "A",
          style: {},
          href: "",
          download: "",
          setAttribute(name, value) {
            this[name] = value;
          },
          click() {
            downloads.push({ filename: this.download, href: this.href });
          },
        };
      }
      throw new Error(`the stub does not create <${tag}>`);
    },
  };

  const win = {
    URL: {
      createObjectURL: (blob) => `blob:stub/${blob.size}`,
      revokeObjectURL: () => {},
    },
    // Re-exported by `browser.js` from `window`; nothing here calls them, and
    // the file's own declarations describe the browser's versions.
    CanvasRenderingContext2D: class {},
    CanvasGradient: class {},
    CanvasPattern: class {},
    Image: class {},
    ImageData: class {},
    Path2D: class {},
    DOMMatrix: class {},
    DOMRect: class {},
    DOMPoint: class {},
  };

  const saved = new Map();
  for (const [name, value] of Object.entries({ window: win, document })) {
    saved.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    globalThis[name] = value;
  }

  return () => {
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete globalThis[name];
    }
  };
}

module.exports = { installDOM, downloads, fakeCanvas };
