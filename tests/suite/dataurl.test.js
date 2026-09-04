// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { loadImage } = require("../../lib"),
  { decodeDataURL } = require("../../lib/urls");

const SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">' +
  '<rect width="4" height="4" fill="red"/></svg>';

// A 1x1 PNG, so the base64 forms decode to something an Image will accept.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  "base64",
);

/** `decodeDataURL` as a promise, so a test can await either outcome. */
const decode = (url) => new Promise((res, rej) => decodeDataURL(url, res, rej));

describe("data URLs are read the way RFC 2397 writes them", () => {
  // Both the media type and `;base64` are optional in the grammar:
  //
  //     dataurl := "data:" [ mediatype ] [ ";base64" ] "," data
  //
  // Requiring either one rejects URLs every browser accepts, and the form
  // without `;base64` is the one an inline SVG is usually written as.
  const cases = [
    [
      "a media type and base64",
      `data:image/png;base64,${PNG.toString("base64")}`,
      PNG,
    ],
    [
      "a media type, a charset and percent-encoding",
      `data:image/svg+xml;charset=utf-8,${encodeURIComponent(SVG)}`,
      Buffer.from(SVG),
    ],
    [
      "a media type and no encoding at all",
      `data:image/svg+xml,${encodeURIComponent(SVG)}`,
      Buffer.from(SVG),
    ],
    [
      "no media type either",
      `data:,${encodeURIComponent("hello")}`,
      Buffer.from("hello"),
    ],
    [
      "a media type longer than forty characters",
      `data:application/vnd.oasis.opendocument.graphics;base64,${PNG.toString("base64")}`,
      PNG,
    ],
  ];

  for (const [shape, url, expected] of cases) {
    test(`decodes ${shape}`, async () => {
      const buffer = await decode(url);
      assert.ok(
        Buffer.from(buffer).equals(expected),
        `${shape}: decoded ${buffer.length} bytes, expected ${expected.length}`,
      );
    });
  }

  test("still refuses something that is not a data URL", async () => {
    await assert.rejects(
      () => decode("https://example.test/cat.png"),
      /valid data URL/,
    );
    await assert.rejects(() => decode("data:no-comma-here"), /valid data URL/);
  });

  test("still refuses a non-string", async () => {
    // @ts-expect-error -- deliberately the wrong type
    await assert.rejects(() => decode(42), /Expected a data URL string/);
  });

  // The other direction: the same URLs through the public loader, since that
  // is where a caller meets this. An SVG written inline is the case that
  // matters -- it is the shape CSS and every "icon as a string" helper emits.
  test("loadImage accepts an SVG data URL with no encoding named", async () => {
    const image = await loadImage(
      `data:image/svg+xml,${encodeURIComponent(SVG)}`,
    );
    assert.deepEqual(
      { width: image.width, height: image.height },
      { width: 4, height: 4 },
    );
  });

  test("loadImage still accepts the charset form", async () => {
    const image = await loadImage(
      `data:image/svg+xml;charset=utf-8,${encodeURIComponent(SVG)}`,
    );
    assert.deepEqual(
      { width: image.width, height: image.height },
      { width: 4, height: 4 },
    );
  });
});
