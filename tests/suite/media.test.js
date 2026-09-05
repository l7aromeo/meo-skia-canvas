// @ts-check

"use strict";

const path = require("path"),
  os = require("os"),
  fs = require("fs"),
  nock = require("nock"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  { pathToFileURL, fileURLToPath } = require("url"),
  {
    Canvas,
    Image,
    ImageData,
    FontLibrary,
    loadImage,
    loadImageData,
  } = require("../../lib");

nock("http://_h_o_s_t_")
  .persist()
  .get(/.*/)
  .reply((uri, requestBody) => {
    try {
      return [200, fs.readFileSync(process.cwd() + uri)];
    } catch {
      return [404, `Failed to load image from "${uri}" (HTTP error 404)`];
    }
  });

describe("Image", () => {
  var PATH = "tests/assets/images/pentagon.png",
    URI = `http://_h_o_s_t_/${PATH}`,
    BUFFER = fs.readFileSync(PATH),
    DATA_URI = `data:image/png;base64,${BUFFER.toString("base64")}`,
    FILE_URL = pathToFileURL(PATH),
    FRESH = { complete: false, width: 0, height: 0 },
    LOADED = { complete: true, width: 125, height: 125 },
    FORMAT = "tests/assets/images/format",
    PARSED = { complete: true, width: 60, height: 60 },
    SVG_PATH = `${FORMAT}.svg`,
    SVG_URI = `http://_h_o_s_t_/${SVG_PATH}`,
    SVG_BUFFER = fs.readFileSync(SVG_PATH),
    SVG_DATA_URI = `data:image/svg;base64,${SVG_BUFFER.toString("base64")}`,
    SVG_FILE_URL = pathToFileURL(SVG_PATH),
    img;

  beforeEach(() => (img = new Image()));

  describe("can initialize bitmaps from", () => {
    test("buffer", async () => {
      img = new Image(BUFFER);
      assert.matchesSubset(img, LOADED);
      assert.equal(img.src, "::Buffer::");

      let fakeSrc = "arbitrary*src*string";
      img = new Image(BUFFER, fakeSrc);
      assert.equal(img.src, fakeSrc);

      img = new Image();
      img.src = BUFFER;
      assert.matchesSubset(img, LOADED);
    });

    test("data uri", () => {
      img.src = DATA_URI;
      assert.matchesSubset(img, LOADED);

      img = new Image(DATA_URI);
      assert.matchesSubset(img, LOADED);
      assert.equal(img.src, DATA_URI);

      let fakeSrc = "arbitrary*src*string";
      img = new Image(DATA_URI, fakeSrc);
      assert.equal(img.src, fakeSrc);
    });

    test("local file", async () => {
      assert.matchesSubset(img, FRESH);
      img.src = PATH;
      await img.decode();
      assert.matchesSubset(img, LOADED);
      assert.equal(img.src, PATH);

      assert.throws(() => new Image(PATH), /Expected a valid data URL/);
    });

    test("file url", async () => {
      assert.matchesSubset(img, FRESH);
      img.src = FILE_URL;
      await img.decode();
      assert.matchesSubset(img, LOADED);
      assert.equal(img.src, fileURLToPath(FILE_URL));

      assert.throws(() => new Image(FILE_URL), /Expected a valid data URL/);
    });

    test("http url", (t, done) => {
      assert.matchesSubset(img, FRESH);
      img.onload = (loaded) => {
        assert.equal(loaded, img);
        assert.matchesSubset(img, LOADED);
        done();
      };
      img.src = URI;

      assert.throws(() => new Image(URI), /Expected a valid data URL/);
    });

    test("loadImage call", async () => {
      assert.matchesSubset(img, FRESH);

      img = await loadImage(URI);
      assert.matchesSubset(img, LOADED);

      img = await loadImage(BUFFER);
      assert.matchesSubset(img, LOADED);

      img = await loadImage(DATA_URI);
      assert.matchesSubset(img, LOADED);

      img = await loadImage(PATH);
      assert.matchesSubset(img, LOADED);

      img = await loadImage(SVG_PATH);
      assert.matchesSubset(img, PARSED);

      img = await loadImage(new URL(URI));
      assert.matchesSubset(img, LOADED);

      img = await loadImage(new URL(DATA_URI));
      assert.matchesSubset(img, LOADED);

      img = await loadImage(pathToFileURL(PATH));
      assert.matchesSubset(img, LOADED);

      img = await loadImage(pathToFileURL(SVG_PATH));
      assert.matchesSubset(img, PARSED);

      await assert.rejects(
        loadImage("http://_h_o_s_t_/nonesuch"),
        /HTTP error 404/,
      );
    });
  });

  describe("can initialize SVGs from", () => {
    test("buffer", () => {
      assert.matchesSubset(img, FRESH);
      img = new Image(SVG_BUFFER);
      assert.matchesSubset(img, PARSED);

      img = new Image();
      img.src = SVG_BUFFER;
      assert.matchesSubset(img, PARSED);
    });

    test("data uri", async () => {
      assert.matchesSubset(img, FRESH);
      img.src = SVG_DATA_URI;
      assert.matchesSubset(img, PARSED);
    });

    test("local file", async () => {
      assert.matchesSubset(img, FRESH);
      img.src = SVG_PATH;
      assert(!img.complete);
      await img.decode();
      assert.matchesSubset(img, PARSED);
    });

    test("file url", async () => {
      assert.matchesSubset(img, FRESH);
      img.src = SVG_FILE_URL;
      assert(!img.complete);
      await img.decode();
      assert.matchesSubset(img, PARSED);
    });

    test("http url", (t, done) => {
      assert.matchesSubset(img, FRESH);
      img.onload = (loaded) => {
        assert.equal(loaded, img);
        assert.matchesSubset(img, PARSED);
        done();
      };
      img.src = SVG_URI;
      assert(!img.complete);
    });
  });

  // What `currentColor` in an SVG resolves to. Asserted at the pixels rather
  // than through the getter, because the getter reports the override and the
  // question is whether the override reached the drawing.
  describe("can recolour an SVG through currentColor", () => {
    const svg = (body) =>
      Buffer.from(
        `<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">${body}</svg>`,
      );
    const CURRENT = `<rect width="4" height="4" fill="currentColor"/>`;
    const firstPixel = (image) => {
      let canvas = new Canvas(4, 4),
        ctx = canvas.getContext("2d");
      ctx.drawImage(image, 0, 0);
      return [...ctx.getImageData(0, 0, 1, 1).data];
    };

    test("with the undertone showing what it replaced", () => {
      // Without the override the initial black is what `currentColor`
      // resolves to, so a test asserting only the red would pass against an
      // implementation that painted red unconditionally.
      let plain = new Image(svg(CURRENT));
      assert.deepEqual(firstPixel(plain), [0, 0, 0, 255]);
      assert.equal(plain.currentColor, null);

      let recoloured = new Image(svg(CURRENT));
      recoloured.currentColor = "red";
      assert.deepEqual(firstPixel(recoloured), [255, 0, 0, 255]);
    });

    test("whether it is set before or after the source", () => {
      // Before the source the document is recorded once with the colour
      // already applied; after it, the recording is replaced. The pixels
      // cannot tell them apart, which is the point -- only the cost differs.
      let after = new Image(svg(CURRENT));
      after.currentColor = "red";

      let before = new Image();
      before.currentColor = "red";
      before.prop("data", svg(CURRENT));

      assert.deepEqual(firstPixel(before), firstPixel(after));
      assert.deepEqual(firstPixel(before), [255, 0, 0, 255]);
    });

    test("and leaves paint that did not ask for it alone", () => {
      // The control that separates this from overwriting every fill.
      let literal = new Image(svg(`<rect width="4" height="4" fill="#0F0"/>`));
      literal.currentColor = "red";
      assert.deepEqual(firstPixel(literal), [0, 255, 0, 255]);

      // And a subtree declaring its own `color` resolves against that, which
      // is inheritance rather than a limit of the override.
      let nested = new Image(svg(`<g color="#00F">${CURRENT}</g>`));
      nested.currentColor = "red";
      assert.deepEqual(firstPixel(nested), [0, 0, 255, 255]);
    });

    test("reporting the override rather than its effect", async () => {
      let img = new Image();
      assert.equal(img.currentColor, null, "null until set");
      // Serialised through the same path `fillStyle` uses, so one library
      // cannot answer two different strings for one colour: hex when the
      // alpha is opaque, `rgba()` with three decimals when it is not.
      let ctx = new Canvas(1, 1).getContext("2d");
      for (let input of ["#0af", "red", "rgba(0,170,255,0.5)"]) {
        ctx.fillStyle = input;
        img.currentColor = input;
        assert.equal(img.currentColor, ctx.fillStyle);
      }
      assert.equal(img.currentColor, "rgba(0, 170, 255, 0.502)");

      // A raster source has nothing for `currentColor` to reach, so the
      // getter says so rather than reporting a colour that did nothing.
      let raster = await loadImage(PATH);
      raster.currentColor = "red";
      assert.equal(raster.currentColor, null);

      // Same for a source that decoded as nothing at all: the getter asks
      // what the content is rather than what it is not, so a broken image
      // does not report an override either.
      let broken = new Image();
      broken.currentColor = "red";
      assert.equal(broken.currentColor, "#ff0000", "nothing loaded yet");
      try {
        broken.prop("data", Buffer.from("not an image, not an svg"));
      } catch {
        // decoding failure is the point; the property read below is the test
      }
      assert.equal(broken.currentColor, null);
    });

    test("and through loadImage's options", async () => {
      let uri = "data:image/svg+xml;base64," + svg(CURRENT).toString("base64");
      let img = await loadImage(uri, { currentColor: "red" });
      assert.deepEqual(firstPixel(img), [255, 0, 0, 255]);
    });
  });

  describe("sends notifications through", () => {
    test(".complete flag", async () => {
      assert(!img.complete);

      img.src = PATH;
      await img.decode();
      assert(img.complete);
    });

    test(".onload callback", (t, done) => {
      // ensure that the fetch process can be overwritten while in flight
      img.onload = (loaded) => {
        throw Error("should not be called");
      };
      img.src = URI;

      img.onload = function () {
        // confirm that `this` is set correctly
        assert.equal(this, img);
        done();
      };
      img.src = "http://_h_o_s_t_/tests/assets/images/globe.jpg";
    });

    test(".onerror callback", (t, done) => {
      img.onerror = (err) => {
        assert.match(err.message, /HTTP error 404/);
        done();
      };
      img.src = "http://_h_o_s_t_/nonesuch";
    });

    test(".decode promise", async () => {
      await assert.rejects(() => img.decode(), /Image source not set/);

      img.src = URI;
      let decoded = await img.decode();
      assert.equal(decoded, img);

      // can load new data into existing Image
      img.src = "http://_h_o_s_t_/tests/assets/images/format.png";
      decoded = await img.decode();
      assert.equal(decoded, img);

      // autoresolves once loaded
      assert.equal(await img.decode(), img);
    });
  });

  describe("can decode format", () => {
    const asBuffer = (path) => fs.readFileSync(path);

    const asDataURI = (path) => {
      let ext = path.split(".").at(-1),
        mime = `image/${ext.replace("jpg", "jpeg")}`,
        content = fs.readFileSync(path).toString("base64");
      return `data:${mime};base64,${content}`;
    };

    async function testFormat(ext) {
      let path = `${FORMAT}.${ext}`;

      let img = new Image();
      img.src = path;
      await img.decode();
      assert.matchesSubset(img, PARSED);

      img = new Image();
      img.src = asDataURI(path);
      await img.decode();
      assert.matchesSubset(img, PARSED);

      img = new Image(asBuffer(path));
      assert.matchesSubset(img, PARSED);
    }

    test("PNG", async () => await testFormat("png"));
    test("JPEG", async () => await testFormat("jpg"));
    test("GIF", async () => await testFormat("gif"));
    test("BMP", async () => await testFormat("bmp"));
    test("ICO", async () => await testFormat("ico"));
    test("WEBP", async () => await testFormat("webp"));
    test("SVG", async () => await testFormat("svg"));
  });

  describe("can read an AVIF, which Skia cannot", () => {
    // Skia ships no AVIF decoder at all -- not for animations and not for
    // stills -- so before this library decoded them itself, `loadImage` of
    // any `.avif` failed outright. That is the format increasingly served
    // to browsers, so the hole was wider than round-tripping our own files.
    const SIZE = 32;
    const drawn = (source) => {
      let canvas = new Canvas(SIZE, SIZE);
      canvas.gpu = false;
      canvas.getContext("2d").drawImage(source, 0, 0);
      return [...canvas.toBufferSync("raw")];
    };

    // `pages` frames, each a different flat colour with a white bar, so a
    // frame returned in the wrong order is visible as the wrong colour.
    const encode = (pages) => {
      let canvas = new Canvas(SIZE, SIZE);
      canvas.gpu = false;
      for (let i = 0; i < pages; i++) {
        let ctx = i ? canvas.newPage() : canvas.getContext("2d");
        ctx.fillStyle = ["#ff0000", "#00ff00", "#0000ff", "#ffff00"][i % 4];
        ctx.fillRect(0, 0, SIZE, SIZE);
        ctx.fillStyle = "#ffffff";
        ctx.fillRect(4, 4, 8, 24);
      }
      let opts = pages > 1 ? { quality: 1, fps: 10 } : { quality: 1 };
      return {
        avif: canvas.toBufferSync("avif", opts),
        pages: Array.from({ length: pages }, (_, i) => [
          ...canvas.toBufferSync("raw", { page: i + 1 }),
        ]),
      };
    };

    test("loads a still through loadImage", async () => {
      let { avif, pages } = encode(1);
      let img = await loadImage(avif);

      assert.equal(img.width, SIZE);
      assert.equal(img.height, SIZE);
      assert.equal(img.complete, true);
      assert.equal(img.frames, 1);
      assert.deepEqual(img.delays, [0]);
      // A still at quality 1.0 round-trips exactly, so equality is the
      // right assertion rather than a tolerance.
      assert.deepEqual(drawn(img), pages[0]);
    });

    test("loads an animation and reaches every frame", async () => {
      let { avif, pages } = encode(4);
      let img = await loadImage(avif);

      assert.equal(img.frames, 4, "one frame per page");
      assert.deepEqual(img.delays, [100, 100, 100, 100]);

      for (let i = 0; i < img.frames; i++) {
        let got = drawn(img.frame(i)),
          want = pages[i];
        assert.equal(got.length, want.length, `frame ${i} size`);
        // Exact for the key frame; within a level after it, because this
        // is coded lossily and the filters run even at quantizer zero.
        let worst = 0;
        for (let n = 0; n < got.length; n++)
          worst = Math.max(worst, Math.abs(got[n] - want[n]));
        assert.ok(worst <= (i === 0 ? 0 : 1), `frame ${i} differs by ${worst}`);
      }
    });

    test("keeps transparency through the auxiliary track", async () => {
      // Alpha is a second coded track. Ignoring it yields a perfectly good
      // opaque animation, so only the pixels report the mistake.
      let canvas = new Canvas(SIZE, SIZE);
      canvas.gpu = false;
      for (let i = 0; i < 3; i++) {
        let ctx = i ? canvas.newPage() : canvas.getContext("2d");
        ctx.clearRect(0, 0, SIZE, SIZE);
        ctx.fillStyle = "#0080ff";
        ctx.fillRect(8, 8, 16, 16);
      }
      let img = await loadImage(
        canvas.toBufferSync("avif", { quality: 1, fps: 10 }),
      );

      assert.equal(img.frames, 3);
      for (let i = 0; i < img.frames; i++) {
        let px = drawn(img.frame(i));
        assert.equal(px[3], 0, `frame ${i} corner should be transparent`);
        let middle = (16 * SIZE + 16) * 4;
        assert.equal(px[middle + 3], 255, `frame ${i} centre should be opaque`);
      }
    });

    test("reads a file another encoder wrote", async () => {
      // Every other test here encodes with this library and reads the
      // result back, which proves the two halves agree with each other and
      // nothing else. `foreign.avif` was written by the AVIF encoder macOS
      // ships, from a canvas this repository drew, and it is the only AVIF
      // in the suite whose bytes this code did not produce.
      //
      // Four solid quadrants and one off-centre white bar, so the pixels
      // report more than "it decoded": a rotation permutes the quadrants
      // and a mirror moves the bar, either of which would otherwise pass as
      // a picture of the right size.
      const QUADRANTS = [
        [128, 128, [208, 32, 32], "top left"],
        [384, 128, [32, 160, 64], "top right"],
        [128, 384, [32, 64, 208], "bottom left"],
        [384, 384, [224, 192, 32], "bottom right"],
      ];
      // The encoder is lossy, and measured at ±1 on these flat fields. The
      // margin is for a different libaom, not for a wrong quadrant -- the
      // colours are 100 or more apart in every channel that separates them.
      const TOLERANCE = 4;

      let img = await loadImage("tests/assets/images/foreign.avif");
      assert.equal(img.width, 512);
      assert.equal(img.height, 512);
      assert.equal(img.frames, 1);

      let canvas = new Canvas(img.width, img.height);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.drawImage(img, 0, 0);
      const at = (x, y) => [...ctx.getImageData(x, y, 1, 1).data];

      for (let [x, y, want, where] of QUADRANTS) {
        let got = at(x, y);
        for (let c = 0; c < want.length; c++)
          assert.ok(
            Math.abs(got[c] - want[c]) <= TOLERANCE,
            `${where} channel ${c}: got ${got[c]}, want ${want[c]}`,
          );
        assert.equal(got[3], 255, `${where} should be opaque`);
      }

      assert.deepEqual(at(60, 30), [255, 255, 255, 255], "the bar is white");
      // Where the bar is not. Reflecting it across either axis lands here,
      // so this is the assertion a mirrored decode fails.
      let bare = at(452, 30);
      assert.ok(
        Math.abs(bare[1] - 160) <= TOLERANCE,
        `mirrored: expected the top-right quadrant, got ${bare.join(" ")}`,
      );
    });

    test("composes a file stored as a grid of tiles", async () => {
      // Past a few hundred pixels Apple's encoder stops writing one coded
      // image and writes a `grid` item arranging several, which is what a
      // photograph off a phone is. The 512-pixel fixture above decoded
      // while this one -- the same picture, twice the size -- did not.
      //
      // The tiles fall on the quadrant boundaries, so one placed in the
      // wrong cell reads as the wrong colour rather than a subtle seam.
      const QUADRANTS = [
        [256, 256, [208, 32, 32], "top left"],
        [768, 256, [32, 160, 64], "top right"],
        [256, 768, [32, 64, 208], "bottom left"],
        [768, 768, [224, 192, 32], "bottom right"],
      ];
      const TOLERANCE = 4;

      let img = await loadImage("tests/assets/images/foreign-grid.avif");
      assert.equal(img.width, 1024);
      assert.equal(img.height, 1024);

      let canvas = new Canvas(img.width, img.height);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.drawImage(img, 0, 0);
      const at = (x, y) => [...ctx.getImageData(x, y, 1, 1).data];

      for (let [x, y, want, where] of QUADRANTS) {
        let got = at(x, y);
        for (let c = 0; c < want.length; c++)
          assert.ok(
            Math.abs(got[c] - want[c]) <= TOLERANCE,
            `${where} channel ${c}: got ${got[c]}, want ${want[c]}`,
          );
      }

      // Either side of the seam between two tiles, where a stride error or
      // a tile written one pixel over shows up first.
      assert.deepEqual(at(511, 256), at(4, 256), "left of the vertical seam");
      assert.deepEqual(at(512, 256), at(1019, 256), "right of it");
    });

    test("codes losslessly when asked, and exactly", async () => {
      // rav1e could not do this at all -- its lossless block is
      // unimplemented, so a quantizer of zero still filtered. libaom has the
      // coding tool, and this asserts equality rather than a tolerance.
      //
      // Lossless needs the identity matrix as well as the flag: without it
      // the RGB is rounded into BT.601 before quantisation ever runs, and
      // the file preserves data that was already lossy.
      const SIDE = 48;
      let canvas = new Canvas(SIDE, SIDE);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ["#ff0000", "#00ff00", "#0000ff"].forEach((col, i) => {
        ctx.fillStyle = col;
        ctx.fillRect(i * 16, 0, 16, SIDE / 2);
      });
      for (let x = 0; x < SIDE; x++) {
        let v = Math.round((x / (SIDE - 1)) * 255);
        ctx.fillStyle = `rgb(${v} ${Math.round(v / 2)} 255)`;
        ctx.fillRect(x, SIDE / 2, 1, SIDE / 2);
      }

      const wanted = [...canvas.toBufferSync("raw")];
      let img = await loadImage(
        canvas.toBufferSync("avif", { lossless: true }),
      );
      let out = new Canvas(SIDE, SIDE);
      out.gpu = false;
      out.getContext("2d").drawImage(img, 0, 0);
      const got = [...out.toBufferSync("raw")];

      assert.equal(got.length, wanted.length);
      let worst = 0;
      for (let i = 0; i < got.length; i++)
        worst = Math.max(worst, Math.abs(got[i] - wanted[i]));
      assert.equal(worst, 0, "lossless should mean lossless, not nearly");

      // Refused where it cannot be honoured, rather than overriding one of
      // the two options the caller named.
      assert.throws(
        () =>
          canvas.toBufferSync("avif", {
            lossless: true,
            chromaSampling: "4:2:0",
          }),
        /lossless/,
      );
      assert.throws(
        () => canvas.toBufferSync("png", { lossless: true }),
        /lossless/,
      );
    });

    test("takes a chromaSampling, and refuses it elsewhere", async () => {
      // Full chroma is the default because this library draws canvases:
      // measured on flat UI with text, "4:2:0" was 22 dB worse *and* made a
      // larger file. On photographs it is 30% smaller for 7 dB, so the
      // choice belongs to the caller.
      //
      // Alternating single-pixel stripes, because 4:2:0 averages chroma over
      // two-by-two cells aligned to even columns -- one wide edge at x = 32
      // falls on a cell boundary and survives untouched.
      const SIDE = 64;
      const draw = () => {
        let canvas = new Canvas(SIDE, SIDE);
        canvas.gpu = false;
        let ctx = canvas.getContext("2d");
        for (let x = 0; x < SIDE; x++) {
          ctx.fillStyle = x % 2 ? "#00ff00" : "#ff0000";
          ctx.fillRect(x, 0, 1, SIDE);
        }
        return canvas;
      };

      const wanted = [...draw().toBufferSync("raw")];
      const error = async (chromaSampling) => {
        let buf = draw().toBufferSync("avif", { quality: 1, chromaSampling });
        let img = await loadImage(buf);
        let out = new Canvas(SIDE, SIDE);
        out.gpu = false;
        out.getContext("2d").drawImage(img, 0, 0);
        let got = [...out.toBufferSync("raw")];
        return got.reduce((sum, v, i) => sum + Math.abs(v - wanted[i]), 0);
      };

      let full = await error("4:4:4");
      let quarter = await error("4:2:0");
      assert.ok(
        quarter > full,
        `4:2:0 should blur the stripes 4:4:4 keeps: ${quarter} against ${full}`,
      );

      // Refused rather than dropped, both ways round: the mistake this
      // prevents is a caller believing a PNG came out subsampled.
      assert.throws(
        () => draw().toBufferSync("png", { chromaSampling: "4:2:0" }),
        /chromaSampling/,
      );
      assert.throws(
        () => draw().toBufferSync("avif", { chromaSampling: "4:1:1" }),
        /4:4:4/,
      );
    });

    test("reads a file in the space its ICC profile names", async () => {
      // The same drawing as `foreign.avif`, converted to Display P3 and
      // carrying that profile in a `colr` box of type `prof`. Its coded
      // values are P3 numbers, so a decoder that discards the profile
      // returns a valid picture of the wrong hue and says nothing.
      //
      // Drawn onto an sRGB canvas it converts back to what was drawn.
      // Measured with the profile deliberately ignored, the top-left
      // quadrant reads 191, 52, 45 against 208, 32, 32 -- twenty levels
      // out, where this allows six.
      const QUADRANTS = [
        [128, 128, [208, 32, 32], "top left"],
        [384, 128, [32, 160, 64], "top right"],
        [128, 384, [32, 64, 208], "bottom left"],
        [384, 384, [224, 192, 32], "bottom right"],
      ];
      const TOLERANCE = 6;

      let img = await loadImage("tests/assets/images/foreign-p3.avif");
      let canvas = new Canvas(img.width, img.height);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.drawImage(img, 0, 0);

      for (let [x, y, want, where] of QUADRANTS) {
        let got = [...ctx.getImageData(x, y, 1, 1).data];
        for (let c = 0; c < want.length; c++)
          assert.ok(
            Math.abs(got[c] - want[c]) <= TOLERANCE,
            `${where} channel ${c}: got ${got[c]}, want ${want[c]} -- the profile looks unread`,
          );
      }
    });
  });

  describe("can reach the frames of an animation", () => {
    // Two pixels wide, three frames, of which the last two cover one pixel
    // each. A frame handed back whole is evidence it was composited against
    // what came before rather than returned as the sub-rectangle it was
    // stored as.
    const ANIMATION = "tests/assets/images/animated.gif";

    // The RGBA of an image drawn 1:1 onto a canvas of its size.
    const drawn = (img) => {
      let canvas = new Canvas(img.width, img.height);
      canvas.getContext("2d").drawImage(img, 0, 0);
      return [...canvas.toBufferSync("raw")];
    };

    test("counting one delay per frame", async () => {
      let img = await loadImage(ANIMATION);
      assert.equal(img.frames, 3);
      // GIF stores hundredths of a second; these come back in milliseconds,
      // as every other timing in this API is.
      assert.deepEqual(img.delays, [100, 200, 350]);
      assert.equal(img.delays.length, img.frames);
    });

    test("treating a still image as one frame of no duration", async () => {
      let img = await loadImage(`${FORMAT}.png`);
      assert.equal(img.frames, 1);
      // Not an empty array: `delays[i]` is valid for every `i` that `frame`
      // accepts, so the two can never disagree about how many there are.
      assert.deepEqual(img.delays, [0]);
      assert.deepEqual(drawn(img.frame(0)), drawn(img));
    });

    test("compositing each frame against the ones before it", async () => {
      let img = await loadImage(ANIMATION);

      // Deliberately backwards: a partial frame decoded on its own would
      // come back one pixel wide, or missing what it never wrote.
      assert.deepEqual(drawn(img.frame(2)), [0, 0, 255, 255, 0, 255, 0, 255]);
      assert.deepEqual(drawn(img.frame(1)), [255, 0, 0, 255, 0, 255, 0, 255]);
      assert.deepEqual(drawn(img.frame(0)), [255, 0, 0, 255, 255, 0, 0, 255]);
    });

    test("drawing the first frame for the image itself", async () => {
      let img = await loadImage(ANIMATION);
      assert.deepEqual(drawn(img), drawn(img.frame(0)));
    });

    test("handing back an Image and not a bare handle", async () => {
      // Built with the constructor rather than wrapped around the boxed
      // struct, so the private fields behind `decode` and `onload` exist.
      let frame = (await loadImage(ANIMATION)).frame(1);
      assert.ok(frame instanceof Image);
      assert.equal(frame.complete, true);
      assert.equal(await frame.decode(), frame);
      // A single frame is a still image, whatever it came out of.
      assert.equal(frame.frames, 1);
      assert.deepEqual(frame.delays, [0]);
    });

    test("counting from the end for a negative index", async () => {
      // The rule `page` follows in the export options, and the one
      // `Array.prototype.at` follows. `-1` used to arrive at the addon as
      // `as usize`, which saturates, so it silently returned frame 0.
      let img = await loadImage(ANIMATION);
      let frames = [0, 1, 2].map((i) => drawn(img.frame(i)));

      assert.deepEqual(drawn(img.frame(-1)), frames[2], "-1 is the last");
      assert.deepEqual(drawn(img.frame(-2)), frames[1]);
      assert.deepEqual(drawn(img.frame(-3)), frames[0], "-frames is the first");
      assert.throws(() => img.frame(-4), /frame -4 is out of range/);

      // A still image has one frame, so -1 is it and -2 is past the start.
      let still = await loadImage(`${FORMAT}.png`);
      assert.deepEqual(drawn(still.frame(-1)), drawn(still));
      assert.throws(() => still.frame(-2), /the image has 1/);
    });

    test("truncating a fractional index the way Array.at does", async () => {
      // `at` truncates toward zero *before* counting from the end, so
      // `at(-1.5)` is the last element rather than the one before it.
      // Resolving first would make the same argument mean different frames
      // depending on the frame count.
      let img = await loadImage(ANIMATION);
      let frames = [0, 1, 2].map((i) => drawn(img.frame(i)));

      for (let index of [0, 1, 2, -1, -2, -3, 1.9, -1.5, -2.9]) {
        assert.deepEqual(
          drawn(img.frame(index)),
          frames.at(Math.trunc(index)),
          `frame(${index})`,
        );
      }
    });

    test("refusing a frame past the last one", async () => {
      let img = await loadImage(ANIMATION);
      assert.throws(() => img.frame(3), /frame 3 is out of range/);
      let still = await loadImage(`${FORMAT}.png`);
      assert.throws(() => still.frame(1), /the image has 1/);
    });
  });
});

describe("ImageData", () => {
  var FORMAT = "tests/assets/images/format.raw",
    RGBA = { width: 60, height: 60, colorType: "rgba" },
    BGRA = { width: 60, height: 60, colorType: "bgra" };

  describe("can be initialized from", () => {
    test("buffer", () => {
      let buffer = fs.readFileSync(FORMAT);
      let imgData = new ImageData(buffer, 60, 60);
      assert.matchesSubset(imgData, RGBA);

      assert.throws(
        () => new ImageData(buffer, 60, 59),
        /ImageData dimensions must match buffer length/,
      );
    });

    test("loadImageData call", async () => {
      await loadImageData(FORMAT, 60, 60).then((imgData) => {
        assert.matchesSubset(imgData, RGBA);
      });
    });

    test("canvas content", () => {
      let canvas = new Canvas(60, 60),
        ctx = canvas.getContext("2d");
      let rgbaData = ctx.getImageData(0, 0, 60, 60);
      assert.matchesSubset(rgbaData, RGBA);
      let bgraData = ctx.getImageData(0, 0, 60, 60, { colorType: "bgra" });
      assert.matchesSubset(bgraData, BGRA);
    });
  });
});

describe("FontLibrary", () => {
  let canvas,
    ctx,
    WIDTH = 512,
    HEIGHT = 512,
    FONTS_DIR = "tests/assets/fonts",
    findFont = (font) => path.join(FONTS_DIR, font);

  beforeEach(() => {
    canvas = new Canvas(WIDTH, HEIGHT);
    ctx = canvas.getContext("2d");
  });

  afterEach(() => {
    FontLibrary.reset();
  });

  test("can list families", () => {
    let fams = FontLibrary.families,
      sorted = fams.slice().sort(),
      unique = [...new Set(sorted)];

    assert(fams.indexOf("Arial") >= 0 || fams.indexOf("DejaVu Sans") >= 0);
    assert.deepEqual(fams, sorted);
    assert.deepEqual(fams, unique);
  });

  test("can check for a family", () => {
    assert(FontLibrary.has("Arial") || FontLibrary.has("DejaVu Sans"));
    assert(!FontLibrary.has("_n_o_n_e_s_u_c_h_"));
  });

  test("can describe a family", () => {
    let fam = FontLibrary.has("Arial")
      ? "Arial"
      : FontLibrary.has("DejaVu Sans")
        ? "DejaVu Sans"
        : null;

    if (fam) {
      let info = FontLibrary.family(fam);
      assert(info);
      assert(Object.hasOwn(info, "family"));
      assert(Object.hasOwn(info, "weights"));
      assert.equal(info && typeof info.weights[0], "number");
      assert(Object.hasOwn(info, "widths"));
      assert.equal(info && typeof info.widths[0], "string");
      assert(Object.hasOwn(info, "styles"));
      assert.equal(info && typeof info.styles[0], "string");
    }
  });

  test("can register fonts", () => {
    let ttf = findFont("AmstelvarAlpha-VF.ttf"),
      name = "AmstelvarAlpha",
      alias = "PseudonymousBosch";

    // with real name
    assert.doesNotThrow(() => FontLibrary.use(ttf));
    assert(FontLibrary.has(name));
    assert.contains((FontLibrary.family(name) || {}).weights, 400);

    // with alias
    assert.doesNotThrow(() => FontLibrary.use(alias, ttf));
    assert(FontLibrary.has(alias));
    assert.contains((FontLibrary.family(alias) || {}).weights, 400);

    // fonts disappear after reset
    FontLibrary.reset();
    assert(!FontLibrary.has(name));
    assert(!FontLibrary.has(alias));
  });

  test("can render woff2 fonts", () => {
    for (const ext of ["woff", "woff2"]) {
      let woff = findFont("Monoton-Regular." + ext),
        name = "Monoton";
      assert.doesNotThrow(() => FontLibrary.use(woff));
      assert(FontLibrary.has(name));

      ctx.font = "256px Monoton";
      ctx.fillText("G", 128, 256);

      // look for one of the gaps between the inline strokes of the G
      let bmp = ctx.getImageData(300, 172, 1, 1);
      assert.deepEqual(Array.from(bmp.data), [0, 0, 0, 0]);
    }
  });

  test("renders a family differently once the library has it", () => {
    // A font is resolved once per canonical string and remembered, because
    // reading the specification behind that string costs about thirty times
    // what parsing it does. A family the library does not have still
    // resolves -- Skia falls back rather than failing -- so the same name
    // means one thing before a `use()` and another after, and remembering
    // must not flatten the two.
    //
    // What makes this hold is that the families, not the typeface, are what
    // a layout resolves against, and they come from the CSS rather than from
    // the library. Pinned all the same: it is the property a cache one layer
    // higher -- one that skipped the call when the string had not changed --
    // would quietly break.
    const woff = findFont("Monoton-Regular.woff");
    const widthOf = (font) => {
      ctx.font = font;
      return ctx.measureText("G").width;
    };

    assert.ok(!FontLibrary.has("Monoton"), "not registered yet");
    const fallback = widthOf("256px Monoton");

    FontLibrary.use(woff);
    assert.notEqual(
      widthOf("256px Monoton"),
      fallback,
      "registering the family changed what the name draws",
    );

    FontLibrary.reset();
    assert.equal(
      widthOf("256px Monoton"),
      fallback,
      "and unregistering it changed the answer back",
    );
  });

  test("applies a remembered font as fully as a fresh one", () => {
    // A hit hands back the whole specification, not a note that nothing
    // changed: everything the font names has to be written again over
    // whatever was set in between.
    FontLibrary.use(findFont("Monoton-Regular.woff"));

    ctx.font = "24px Monoton";
    const width = ctx.measureText("MMM").width;

    ctx.fontStretch = "condensed";
    assert.equal(ctx.fontStretch, "condensed");

    ctx.font = "24px Monoton"; // the same string, now a cache hit
    assert.equal(ctx.fontStretch, "normal", "the stretch the font names");
    assert.equal(ctx.measureText("MMM").width, width, "and the same metrics");
  });

  test("does not fake a weight or a slant a family does not have", () => {
    // Skia's paragraph builder synthesises a bold or an oblique when the
    // face it finds is not the one asked for, so the character style is
    // pinned to what the match actually reports. Monoton has one face, so
    // asking it for bold is asking for the synthesis.
    //
    // That pin used to be found by searching the font collection a second
    // time, inside `layout`, on every call; it comes back with the
    // collection now. This is what says it still arrives -- without it all
    // three of these render differently from the plain one.
    FontLibrary.use(findFont("Monoton-Regular.woff"));
    const drawn = (font) => {
      const canvas = new Canvas(300, 80);
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      ctx.font = font;
      ctx.fillText("Hamburg", 4, 50);
      return canvas.toBufferSync("raw").toString("base64");
    };

    const plain = drawn("32px Monoton");
    for (const font of [
      "bold 32px Monoton",
      "900 32px Monoton",
      "italic 32px Monoton",
    ]) {
      assert.equal(drawn(font), plain, `${font} was synthesised`);
    }
  });

  test("lays a variable font out at the axis it was asked for", () => {
    // Two things have to agree for this: the collection handed to the
    // paragraph builder holds a typeface instanced at the requested axes,
    // and the character style is pinned to what that instance reports, so
    // Skia lays out the real weight rather than synthesising one over the
    // master. They come from one search now; they used to come from two, and
    // only the second of them looked at the instanced collection.
    FontLibrary.use(findFont("AmstelvarAlpha-VF.ttf"));
    const widthOf = (font, variations) => {
      const ctx = new Canvas(WIDTH, HEIGHT).getContext("2d");
      ctx.font = font;
      if (variations) ctx.fontVariationSettings = variations;
      return ctx.measureText("Hamburgefonstiv").width;
    };

    const light = widthOf("300 24px AmstelvarAlpha");
    const heavy = widthOf("800 24px AmstelvarAlpha");
    assert.ok(light > 0 && heavy > 0, "the family was found at all");
    assert.notEqual(light, heavy, "the weight reached the wght axis");

    // And an explicit axis, which takes the same route by another door.
    // Quoted tags: that is the CSS syntax, and an unquoted one is ignored
    // the way the specification says an invalid value should be -- which is
    // silent, and is why this reads as a font that has no axes at all.
    const narrow = widthOf("24px AmstelvarAlpha", '"wdth" 70');
    const wide = widthOf("24px AmstelvarAlpha", '"wdth" 130');
    assert.notEqual(narrow, wide, "an explicit axis reached it too");
    assert.notEqual(
      widthOf("24px AmstelvarAlpha", '"opsz" 8'),
      widthOf("24px AmstelvarAlpha", '"opsz" 144'),
      "and an axis with no CSS property of its own",
    );
  });

  test("can handle different use() signatures", () => {
    const normalizePath = (p) =>
      os.platform() == "win32"
        ? p
            .replace(/^\\\\(?<path>[.?])/, "//$1") // The device path (\\.\ or \\?\)
            .replaceAll(/\\(?![!()+@[\]{}])/g, "/") // All backslashes except escapes
        : p;

    FONTS_DIR = normalizePath(FONTS_DIR);

    const amstel = `${FONTS_DIR}/AmstelvarAlpha-VF.ttf`;
    const monoton = [
      `${FONTS_DIR}/Monoton-Regular.woff`,
      `${FONTS_DIR}/Monoton-Regular.woff2`,
    ];
    const montserrat = [
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-200.woff2`,
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-700italic.woff2`,
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-200italic.woff2`,
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-italic.woff2`,
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-700.woff2`,
      `${FONTS_DIR}/montserrat-latin/montserrat-v30-latin-regular.woff2`,
    ];

    // list with multiple families
    assert.equal(FontLibrary.use([amstel, ...monoton]).length, 3);

    // alias for single family
    assert.equal(FontLibrary.use("Montmartre", montserrat).length, 6);

    // multiple family aliases (single-face per family)
    let single = FontLibrary.use({
      Monaton: monoton[0],
      Montserrat: montserrat[0],
    });
    assert.equal((single.Monaton || []).length, 1);
    assert.equal((single.Montserrat || []).length, 1);

    // multiple aliases (lists of faces)
    let multiple = FontLibrary.use({
      Monaton: [monoton[1]],
      Montserrat: montserrat.slice(1, -1),
    });
    assert.equal((multiple.Monaton || []).length, 1);
    assert.equal((multiple.Montserrat || []).length, 4);
  });
});
