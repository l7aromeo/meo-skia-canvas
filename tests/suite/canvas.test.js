// @ts-check

"use strict";

const fs = require("fs"),
  tmp = require("tmp"),
  path = require("path"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  { Canvas, Image, loadImage, backend } = require("../../lib"),
  { skiaNode, core } = require("../../lib/classes/neon");

const BLACK = [0, 0, 0, 255],
  WHITE = [255, 255, 255, 255],
  CLEAR = [0, 0, 0, 0],
  MAGIC = {
    jpg: Buffer.from([0xff, 0xd8, 0xff]),
    png: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    webp: Buffer.from([0x52, 0x49, 0x46, 0x46]),
    pdf: Buffer.from([0x25, 0x50, 0x44, 0x46, 0x2d]),
    svg: Buffer.from(`<?xml version`, "utf-8"),
  },
  MIME = {
    png: "image/png",
    jpg: "image/jpeg",
    webp: "image/webp",
    pdf: "application/pdf",
    svg: "image/svg+xml",
  };

describe("Canvas", () => {
  let canvas,
    ctx,
    WIDTH = 512,
    HEIGHT = 512,
    pixel = (x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data);

  let TMP,
    tmpFiles = () =>
      fs
        .readdirSync(TMP)
        .map((fn) => path.join(TMP, fn))
        .filter((fn) => fs.lstatSync(fn).isFile());

  beforeEach(() => {
    canvas = new Canvas(WIDTH, HEIGHT);
    ctx = canvas.getContext("2d");
  });

  describe("can get & set", () => {
    test("width & height", () => {
      assert.equal(canvas.width, WIDTH);
      assert.equal(canvas.height, HEIGHT);

      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);
      assert.equal(ctx.fillStyle, "#ffffff");
      assert.deepEqual(pixel(0, 0), WHITE);

      // resizing also clears content & resets state
      canvas.width = 123;
      canvas.height = 456;
      assert.equal(canvas.width, 123);
      assert.equal(canvas.height, 456);
      assert.equal(ctx.fillStyle, "#000000");
      assert.deepEqual(pixel(0, 0), CLEAR);
    });
  });

  describe("handles bad arguments for", () => {
    beforeEach(() => (TMP = tmp.dirSync().name));
    afterEach(() => fs.rmSync(TMP, { recursive: true }));

    test("initial dimensions", () => {
      let W = 300,
        H = 150,
        c;

      c = new Canvas();
      assert.equal(c.width, W);
      assert.equal(c.height, H);

      c = new Canvas(0, 0);
      assert.equal(c.width, 0);
      assert.equal(c.height, 0);

      c = new Canvas(-99, 123);
      assert.equal(c.width, W);
      assert.equal(c.height, 123);

      c = new Canvas(456);
      assert.equal(c.width, 456);
      assert.equal(c.height, H);

      // @ts-expect-error
      c = new Canvas("0xff");
      assert.equal(c.width, 255);
      assert.equal(c.height, H);

      c = new Canvas(undefined, 789);
      assert.equal(c.width, W);
      assert.equal(c.height, 789);

      // @ts-expect-error
      c = new Canvas("garbage", NaN);
      assert.equal(c.width, W);
      assert.equal(c.height, H);

      // @ts-expect-error
      c = new Canvas(true, {});
      assert.equal(c.width, 1);
      assert.equal(c.height, H);
    });

    test("new page dimensions", () => {
      assert.equal(canvas.width, WIDTH);
      assert.equal(canvas.height, HEIGHT);
      assert.equal(canvas.pages.length, 1);
      canvas.getContext();
      assert.equal(canvas.pages.length, 1);
      canvas.newPage();
      assert.equal(canvas.pages.length, 2);

      let W = 300,
        H = 150,
        c,
        pg;

      c = new Canvas(123, 456);
      assert.equal(c.width, 123);
      assert.equal(c.height, 456);

      assert.equal(c.pages.length, 0);
      pg = c.newPage().canvas;
      assert.equal(c.pages.length, 1);
      c.getContext();
      assert.equal(c.pages.length, 1);

      assert.equal(pg.width, 123);
      assert.equal(pg.height, 456);

      // A lone dimension is refused rather than dropped. This used to add a
      // page at the previous size and say nothing, so `newPage(987)` looked
      // like it had worked.
      assert.throws(() => c.newPage(987), TypeError);
      assert.equal(c.pages.length, 1);

      pg = c.newPage(NaN, NaN).canvas;
      assert.equal(pg.width, W);
      assert.equal(pg.height, H);
    });

    test("export file formats", async () => {
      assert.throws(
        () => canvas.toFile(`${TMP}/output.targa`),
        /Unsupported file format/,
      );
      assert.throws(
        () => canvas.toFile(`${TMP}/output`),
        /Cannot determine image format/,
      );
      assert.throws(
        () => canvas.toFile(`${TMP}/`),
        /Cannot determine image format/,
      );
      await canvas.toFile(`${TMP}/output`, { format: "png" });
    });
  });

  describe("can create | async", () => {
    beforeEach(() => {
      TMP = tmp.dirSync().name;

      ctx.fillStyle = "red";
      ctx.arc(100, 100, 25, 0, Math.PI / 2);
      ctx.fill();
    });
    afterEach(() => fs.rmSync(TMP, { recursive: true }));

    test("JPEGs", async () => {
      await Promise.all([
        canvas.toFile(`${TMP}/output1.jpg`),
        canvas.toFile(`${TMP}/output2.jpeg`),
        canvas.toFile(`${TMP}/output3.JPG`),
        canvas.toFile(`${TMP}/output4.JPEG`),
        canvas.toFile(`${TMP}/output5`, { format: "jpg" }),
        canvas.toFile(`${TMP}/output6`, { format: "jpeg" }),
        canvas.toFile(`${TMP}/output6.png`, { format: "jpeg" }),
      ]);

      let magic = MAGIC.jpg;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("PNGs", async () => {
      await Promise.all([
        canvas.toFile(`${TMP}/output1.png`),
        canvas.toFile(`${TMP}/output2.PNG`),
        canvas.toFile(`${TMP}/output3`, { format: "png" }),
        canvas.toFile(`${TMP}/output4.svg`, { format: "png" }),
      ]);

      let magic = MAGIC.png;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("WEBPs", async () => {
      await Promise.all([
        canvas.toFile(`${TMP}/output1.webp`),
        canvas.toFile(`${TMP}/output2.WEBP`),
        canvas.toFile(`${TMP}/output3`, { format: "webp" }),
        canvas.toFile(`${TMP}/output4.svg`, { format: "webp" }),
      ]);

      let magic = MAGIC.webp;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("SVGs", async () => {
      await Promise.all([
        canvas.toFile(`${TMP}/output1.svg`),
        canvas.toFile(`${TMP}/output2.SVG`),
        canvas.toFile(`${TMP}/output3`, { format: "svg" }),
        canvas.toFile(`${TMP}/output4.jpeg`, { format: "svg" }),
      ]);

      for (let path of tmpFiles()) {
        let svg = fs.readFileSync(path, "utf-8");
        assert.match(svg, /^<\?xml version/);
      }
    });

    test("PDFs", async () => {
      await Promise.all([
        canvas.toFile(`${TMP}/output1.pdf`),
        canvas.toFile(`${TMP}/output2.PDF`),
        canvas.toFile(`${TMP}/output3`, { format: "pdf" }),
        canvas.toFile(`${TMP}/output4.jpg`, { format: "pdf" }),
      ]);

      let magic = MAGIC.pdf;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("raw pixel buffers", async () => {
      canvas.width = canvas.height = 4;
      ctx.fillStyle = "#f00";
      ctx.fillRect(0, 0, 1, 1);
      ctx.fillStyle = "#0f0";
      ctx.fillRect(1, 0, 1, 1);
      ctx.fillStyle = "#00f";
      ctx.fillRect(0, 1, 1, 1);
      ctx.fillStyle = "#fff";
      ctx.fillRect(1, 1, 1, 1);

      let rgba = ctx.getImageData(0, 0, 2, 2);
      assert.deepEqual(
        rgba.data,
        new Uint8ClampedArray([
          255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ]),
      );

      let bgra = ctx.getImageData(0, 0, 2, 2, { colorType: "bgra" });
      assert.deepEqual(
        bgra.data,
        new Uint8ClampedArray([
          0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
        ]),
      );
    });

    test("raw buffers honour the requested color space", () => {
      // The raw export pinned its readback destination to sRGB, so
      // `toBuffer("raw", {colorSpace})` converted everything back down to it
      // and returned sRGB bytes whatever was asked for -- while
      // `getImageData`, which shares the job, passed the option through. The
      // two disagreed about the same picture.
      let wide = new Canvas(2, 2, { colorSpace: "display-p3" });
      let wideCtx = wide.getContext("2d");
      wideCtx.fillStyle = "rgb(255 0 0)";
      wideCtx.fillRect(0, 0, 2, 2);

      // sRGB red sits inside the P3 gamut, so naming it in P3 coordinates
      // gives a smaller number -- the conversion the destination asks for.
      let raw = Array.from(
        wide.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
      );
      let read = Array.from(
        wideCtx.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data,
      );
      assert.deepEqual(raw, read, "the two readback paths agree");
      assert.deepEqual(raw, [234, 51, 35, 255]);

      // And a colour named in the canvas's own space survives it whole.
      wideCtx.fillStyle = [1, 0, 0, 1];
      wideCtx.fillRect(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(
          wide.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        ),
        [255, 0, 0, 255],
        "P3 red is not clipped to sRGB on the way out",
      );
    });

    test("the canvas composites in the space it was constructed with", () => {
      // A canvas's color space is fixed when it is made, and an export
      // converts out of it -- the way a browser's context does. It used to be
      // whatever the *export call* asked for, which made the compositing
      // space a property of the call rather than of the canvas.
      let wideRed = [1, 0, 0, 1]; // named in the canvas's own space

      let wide = new Canvas(2, 2, { colorSpace: "display-p3" });
      let wideCtx = wide.getContext("2d");
      wideCtx.fillStyle = wideRed;
      wideCtx.fillRect(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(
          wide.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        ),
        [255, 0, 0, 255],
        "P3 red drawn on a P3 canvas keeps its gamut",
      );

      // The same call on an sRGB canvas cannot hold that colour: it is
      // clipped as it is drawn, and asking for P3 on the way out cannot put
      // back what the surface never held.
      let narrow = new Canvas(2, 2, { colorSpace: "srgb" });
      let narrowCtx = narrow.getContext("2d");
      narrowCtx.fillStyle = wideRed;
      narrowCtx.fillRect(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(
          narrow.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        ),
        [234, 51, 35, 255],
        "an sRGB canvas clips at the draw, not at the export",
      );
    });

    test("a wide image is clipped by a narrow canvas", async () => {
      // The case that tells a fixed compositing space from a late-bound one:
      // P3 red has no sRGB spelling, so drawing it into an sRGB canvas has to
      // clip it at the draw. If the surface followed the *export* instead,
      // asking for P3 on the way out would smuggle the colour back through
      // unclipped.
      let source = new Canvas(2, 2, { colorSpace: "display-p3" });
      let sourceCtx = source.getContext("2d");
      sourceCtx.fillStyle = [1, 0, 0, 1];
      sourceCtx.fillRect(0, 0, 2, 2);
      let wideImage = await loadImage(source.toBufferSync("png"));

      let narrow = new Canvas(2, 2, { colorSpace: "srgb" });
      narrow.getContext("2d").drawImage(wideImage, 0, 0);
      assert.deepEqual(
        Array.from(
          narrow.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        ),
        [234, 51, 35, 255],
        "clipped to sRGB as it was drawn",
      );
      // Through the other readback too, on a canvas of its own: the
      // rasterising surface `getImageData` builds has to take the canvas's
      // space for the same reason, and reusing the one above would read a
      // surface that was already built.
      let alsoNarrow = new Canvas(2, 2, { colorSpace: "srgb" });
      let alsoCtx = alsoNarrow.getContext("2d");
      alsoCtx.drawImage(wideImage, 0, 0);
      assert.deepEqual(
        Array.from(
          alsoCtx.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data,
        ),
        [234, 51, 35, 255],
        "and the same through getImageData",
      );

      let wide = new Canvas(2, 2, { colorSpace: "display-p3" });
      wide.getContext("2d").drawImage(wideImage, 0, 0);
      assert.deepEqual(
        Array.from(
          wide.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        ),
        [255, 0, 0, 255],
        "and kept whole by a canvas wide enough to hold it",
      );
    });

    test("a readback inherits the canvas's space", () => {
      // What a browser does: `getImageData()` on a Display P3 canvas hands
      // back P3 components and says so through `ImageData.colorSpace`. This
      // hard-coded sRGB, so it converted the pixels down without being asked
      // -- while every export inherited the space.
      let canvas = new Canvas(2, 2, { colorSpace: "display-p3" });
      let ctx2 = canvas.getContext("2d");
      ctx2.fillStyle = "rgb(255 0 0)";
      ctx2.fillRect(0, 0, 2, 2);

      let inherited = ctx2.getImageData(0, 0, 1, 1);
      assert.equal(inherited.colorSpace, "display-p3");
      assert.deepEqual(
        Array.from(inherited.data),
        [234, 51, 35, 255],
        "sRGB red expressed in the canvas's own space",
      );

      // A call that names a space still wins.
      assert.deepEqual(
        Array.from(ctx2.getImageData(0, 0, 1, 1, { colorSpace: "srgb" }).data),
        [255, 0, 0, 255],
      );

      // And an sRGB canvas is unaffected.
      let plain = new Canvas(2, 2);
      let plainCtx = plain.getContext("2d");
      plainCtx.fillStyle = "rgb(255 0 0)";
      plainCtx.fillRect(0, 0, 2, 2);
      let plainData = plainCtx.getImageData(0, 0, 1, 1);
      assert.equal(plainData.colorSpace, "srgb");
      assert.deepEqual(Array.from(plainData.data), [255, 0, 0, 255]);
    });

    test("exports convert into the space they are asked for", () => {
      // The encoder tags with whatever the image carries, so without a
      // conversion a P3 export of an sRGB canvas came out sRGB -- profile and
      // all -- and an sRGB export of a P3 canvas stayed P3.
      let iccp = (buffer) => buffer.includes(Buffer.from("iCCP"));
      let png = (canvasSpace, exportSpace) => {
        let canvas = new Canvas(2, 2, { colorSpace: canvasSpace });
        let ctx = canvas.getContext("2d");
        ctx.fillStyle = "rgb(255 0 0)";
        ctx.fillRect(0, 0, 2, 2);
        return canvas.toBufferSync("png", { colorSpace: exportSpace });
      };

      assert.ok(!iccp(png("srgb", "srgb")), "sRGB out of sRGB carries none");
      assert.ok(iccp(png("srgb", "display-p3")), "converted up and tagged");
      assert.ok(iccp(png("display-p3", "display-p3")), "P3 out of P3");
      assert.ok(
        !iccp(png("display-p3", "srgb")),
        "and converted back down again",
      );
    });

    test("image-sequences", async () => {
      let colors = ["orange", "yellow", "green", "skyblue", "purple"];
      colors.forEach((color, i) => {
        let dim = 512 + 100 * i;
        ctx = i ? canvas.newPage(dim, dim) : canvas.newPage();
        ctx.fillStyle = color;
        ctx.arc(100, 100, 25, 0, Math.PI + (Math.PI / colors.length) * (i + 1));
        ctx.fill();
        assert.equal(ctx.canvas.height, dim);
        assert.equal(ctx.canvas.width, dim);
      });

      await canvas.toFile(`${TMP}/output-{2}.png`);

      let files = tmpFiles();
      assert.equal(files.length, colors.length + 1);

      for (const [i, fn] of files.entries()) {
        let img = new Image();
        img.src = fn;
        await img.decode();
        assert.equal(img.complete, true);

        // second page inherits the first's size, then they increase
        let dim = i < 2 ? 512 : 512 + 100 * (i - 1);
        assert.equal(img.width, dim);
        assert.equal(img.height, dim);
      }
    });

    test("multi-page PDFs", async () => {
      let colors = ["orange", "yellow", "green", "skyblue", "purple"];
      colors.forEach((color, i) => {
        ctx = canvas.newPage();
        ctx.fillStyle = color;
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        ctx.fillStyle = "white";
        ctx.textAlign = "center";
        ctx.fillText(i + 1, canvas.width / 2, canvas.height / 2);
      });

      let path = `${TMP}/multipage.pdf`;
      await canvas.toFile(path);

      let header = fs.readFileSync(path).slice(0, MAGIC.pdf.length);
      assert(header.equals(MAGIC.pdf));
    });

    test("image Buffers", async () => {
      for (let ext of ["png", "jpg", "pdf", "svg"]) {
        // use extension to specify type
        let path = `${TMP}/output.${ext}`;
        let buf = await canvas.toBuffer(ext);
        assert(buf instanceof Buffer);

        fs.writeFileSync(path, buf);
        let header = fs.readFileSync(path).slice(0, MAGIC[ext].length);
        assert(header.equals(MAGIC[ext]));

        // use mime to specify type
        path = `${TMP}/bymime.${ext}`;
        buf = await canvas.toBuffer(MIME[ext]);
        assert(buf instanceof Buffer);

        fs.writeFileSync(path, buf);
        header = fs.readFileSync(path).slice(0, MAGIC[ext].length);
        assert(header.equals(MAGIC[ext]));
      }
    });

    test("data URLs", async () => {
      for (let ext in MIME) {
        let magic = MAGIC[ext],
          mime = MIME[ext],
          [extURL, mimeURL] = await Promise.all([
            canvas.toDataURL(ext),
            canvas.toDataURL(mime),
          ]),
          header = `data:${mime};base64,`,
          data = Buffer.from(extURL.substr(header.length), "base64");
        assert.equal(extURL, mimeURL);
        assert.equal(extURL.startsWith(header), true);
        assert(data.slice(0, magic.length).equals(magic));
      }
    });

    test("sensible error messages", async () => {
      ctx.fillStyle = "lightskyblue";
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      // invalid path
      await assert.rejects(
        canvas.toFile(`${TMP}/deep/path/that/doesn/not/exist.pdf`),
      );

      // canvas has a zero dimension
      let width = 0,
        height = 128;
      Object.assign(canvas, { width, height });
      assert.matchesSubset(canvas, { width, height });
      await assert.rejects(
        canvas.toFile(`${TMP}/zeroed.png`),
        /must be non-zero/,
      );
    });
  });

  describe("can create | sync", () => {
    beforeEach(() => {
      TMP = tmp.dirSync().name;

      ctx.fillStyle = "red";
      ctx.arc(100, 100, 25, 0, Math.PI / 2);
      ctx.fill();
    });
    afterEach(() => fs.rmSync(TMP, { recursive: true }));

    test("JPEGs", () => {
      canvas.toFileSync(`${TMP}/output1.jpg`);
      canvas.toFileSync(`${TMP}/output2.jpeg`);
      canvas.toFileSync(`${TMP}/output3.JPG`);
      canvas.toFileSync(`${TMP}/output4.JPEG`);
      canvas.toFileSync(`${TMP}/output5`, { format: "jpg" });
      canvas.toFileSync(`${TMP}/output6`, { format: "jpeg" });
      canvas.toFileSync(`${TMP}/output6.png`, { format: "jpeg" });

      let magic = MAGIC.jpg;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("PNGs", () => {
      canvas.toFileSync(`${TMP}/output1.png`);
      canvas.toFileSync(`${TMP}/output2.PNG`);
      canvas.toFileSync(`${TMP}/output3`, { format: "png" });
      canvas.toFileSync(`${TMP}/output4.svg`, { format: "png" });

      let magic = MAGIC.png;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("WEBPs", async () => {
      await Promise.all([
        canvas.toFileSync(`${TMP}/output1.webp`),
        canvas.toFileSync(`${TMP}/output2.WEBP`),
        canvas.toFileSync(`${TMP}/output3`, { format: "webp" }),
        canvas.toFileSync(`${TMP}/output4.svg`, { format: "webp" }),
      ]);

      let magic = MAGIC.webp;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("SVGs", () => {
      canvas.toFileSync(`${TMP}/output1.svg`);
      canvas.toFileSync(`${TMP}/output2.SVG`);
      canvas.toFileSync(`${TMP}/output3`, { format: "svg" });
      canvas.toFileSync(`${TMP}/output4.jpeg`, { format: "svg" });

      for (let path of tmpFiles()) {
        let svg = fs.readFileSync(path, "utf-8");
        assert.match(svg, /^<\?xml version/);
      }
    });

    test("SVGs embed what they cannot describe", () => {
      // Skia's SVG backend writes solid colors, linear and radial gradients
      // and image shaders, and drops everything else without a word: a conic
      // gradient came out as a path with no fill at all, which renders black.
      // Those draws are rasterized into the document instead, and the rest of
      // the page stays vector.
      let plain = new Canvas(60, 60),
        ctx = plain.getContext("2d");
      ctx.fillStyle = "orange";
      ctx.fillRect(10, 10, 40, 40);
      let vector = plain.toBufferSync("svg").toString("utf-8");
      assert.match(vector, /<path/);
      assert.doesNotMatch(vector, /<image/);

      let conic = new Canvas(60, 60);
      ctx = conic.getContext("2d");
      let gradient = ctx.createConicGradient(0, 30, 30);
      gradient.addColorStop(0, "red");
      gradient.addColorStop(1, "blue");
      ctx.fillStyle = gradient;
      ctx.fillRect(10, 10, 40, 40);
      ctx.fillStyle = "orange";
      ctx.fillRect(0, 0, 5, 5);
      let mixed = conic.toBufferSync("svg").toString("utf-8");
      assert.match(mixed, /<image/);
      assert.match(mixed, /<path/);
    });

    test("PDFs", () => {
      canvas.toFileSync(`${TMP}/output1.pdf`);
      canvas.toFileSync(`${TMP}/output2.PDF`);
      canvas.toFileSync(`${TMP}/output3`, { format: "pdf" });
      canvas.toFileSync(`${TMP}/output4.jpg`, { format: "pdf" });

      let magic = MAGIC.pdf;
      for (let path of tmpFiles()) {
        let header = fs.readFileSync(path).slice(0, magic.length);
        assert(header.equals(magic));
      }
    });

    test("image-sequences", async () => {
      let colors = ["orange", "yellow", "green", "skyblue", "purple"];
      colors.forEach((color, i) => {
        let dim = 512 + 100 * i;
        ctx = i ? canvas.newPage(dim, dim) : canvas.newPage();
        ctx.fillStyle = color;
        ctx.arc(100, 100, 25, 0, Math.PI + (Math.PI / colors.length) * (i + 1));
        ctx.fill();
        assert.equal(ctx.canvas.height, dim);
        assert.equal(ctx.canvas.width, dim);
      });

      canvas.toFileSync(`${TMP}/output-{2}.png`);

      let files = tmpFiles();
      assert.equal(files.length, colors.length + 1);

      for (const [i, fn] of files.entries()) {
        let img = new Image();
        img.src = fn;
        await img.decode();
        assert.equal(img.complete, true);

        // second page inherits the first's size, then they increase
        let dim = i < 2 ? 512 : 512 + 100 * (i - 1);
        assert.equal(img.width, dim);
        assert.equal(img.height, dim);
      }
    });

    test("multi-page PDFs", () => {
      let colors = ["orange", "yellow", "green", "skyblue", "purple"];
      colors.forEach((color, i) => {
        ctx = canvas.newPage();
        ctx.fillStyle = color;
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        ctx.fillStyle = "white";
        ctx.textAlign = "center";
        ctx.fillText(i + 1, canvas.width / 2, canvas.height / 2);
      });

      let path = `${TMP}/multipage.pdf`;
      assert.doesNotThrow(() => canvas.toFileSync(path));

      let header = fs.readFileSync(path).slice(0, MAGIC.pdf.length);
      assert(header.equals(MAGIC.pdf));
    });

    test("image Buffers", () => {
      for (let ext of ["png", "jpg", "pdf", "svg"]) {
        // use extension to specify type
        let path = `${TMP}/output.${ext}`;
        let buf = canvas.toBufferSync(ext);
        assert(buf instanceof Buffer);

        fs.writeFileSync(path, buf);
        let header = fs.readFileSync(path).slice(0, MAGIC[ext].length);
        assert(header.equals(MAGIC[ext]));

        // use mime to specify type
        path = `${TMP}/bymime.${ext}`;
        buf = canvas.toBufferSync(MIME[ext]);
        assert(buf instanceof Buffer);

        fs.writeFileSync(path, buf);
        header = fs.readFileSync(path).slice(0, MAGIC[ext].length);
        assert(header.equals(MAGIC[ext]));
      }
    });

    test("data URLs", async () => {
      for (let ext in MIME) {
        let magic = MAGIC[ext],
          mime = MIME[ext],
          extURL = canvas.toURLSync(ext),
          mimeURL = canvas.toURLSync(mime),
          stdURL = canvas.toDataURL(mime, 0.92),
          asyncURL = await canvas.toURL(ext),
          header = `data:${mime};base64,`,
          data = Buffer.from(extURL.substr(header.length), "base64");
        assert.equal(extURL, mimeURL);
        assert.equal(extURL, stdURL);
        assert.equal(extURL, asyncURL);
        assert(extURL.startsWith(header));
        assert(data.slice(0, magic.length).equals(magic));
      }
    });

    test("sensible error messages", () => {
      ctx.fillStyle = "lightskyblue";
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      // invalid path
      assert.throws(() =>
        canvas.toFileSync(`${TMP}/deep/path/that/doesn/not/exist.pdf`),
      );

      // canvas has a zero dimension
      let width = 0,
        height = 128;
      Object.assign(canvas, { width, height });
      assert.matchesSubset(canvas, { width, height });
      assert.throws(
        () => canvas.toFileSync(`${TMP}/zeroed.png`),
        /must be non-zero/,
      );
    });

    test("an image even without a ctx", () => {
      let canvas = new Canvas(200, 200);
      assert.doesNotThrow(() => canvas.toURLSync("png"));
    });
  });
});

describe("backend", () => {
  test("returns backend info without creating a canvas", () => {
    const info = backend();

    // Must have required fields
    assert.ok(info.renderer, "renderer field is required");
    assert.ok(
      ["CPU", "GPU"].includes(info.renderer),
      "renderer must be CPU or GPU",
    );
    assert.ok(typeof info.threads === "number", "threads must be a number");
    assert.ok(info.threads > 0, "threads must be positive");
    assert.ok(
      typeof info.gpuAvailable === "boolean",
      "gpuAvailable must be a boolean",
    );

    // GPU-specific fields
    if (info.renderer === "GPU") {
      assert.ok(
        ["Vulkan", "Metal"].includes(info.api),
        "GPU api must be Vulkan or Metal",
      );
      assert.ok(typeof info.device === "string", "device must be a string");
    }
  });

  test("gpuAvailable matches renderer", () => {
    const info = backend();
    if (info.renderer === "GPU") {
      assert.strictEqual(
        info.gpuAvailable,
        true,
        "gpuAvailable should be true when renderer is GPU",
      );
    }
  });
});

// The format table lives in Rust, and the binding asks for it rather than
// keeping a second copy. These pin the asking: each one fails if the
// JavaScript side goes back to a hand-written list, because a format added on
// the Rust side would then be missing here.
describe("format table", () => {
  const DESCRIBED = JSON.parse(skiaNode.formats());

  /// Three pages, so a format that spans them has something to gather.
  const pages = () => {
    let canvas = new Canvas(4, 2);
    for (let i = 0; i < 3; i++) {
      let ctx = i ? canvas.newPage() : canvas.getContext("2d");
      ctx.fillStyle = ["red", "lime", "blue"][i];
      ctx.fillRect(0, 0, 4, 2);
    }
    return canvas;
  };
  let TMP;
  beforeEach(() => {
    TMP = tmp.dirSync().name;
  });

  test("describes every format with the fields the binding needs", () => {
    assert.ok(DESCRIBED.length >= 6, "should describe at least the six");
    for (let format of DESCRIBED) {
      assert.deepStrictEqual(Object.keys(format).sort(), [
        "aliases",
        "animated",
        "extension",
        "inferable",
        "mime",
        "name",
        "spansPages",
      ]);
      assert.ok(format.name && format.mime && format.extension);
      assert.ok(Array.isArray(format.aliases));
      assert.equal(typeof format.spansPages, "boolean");
      assert.equal(typeof format.animated, "boolean");
      assert.equal(typeof format.inferable, "boolean");
    }
  });

  test("is the same set of names the type declarations offer", () => {
    // `lib/index.d.ts` is written by hand and the addon's table is not, so
    // this is the pair most able to drift apart in silence -- and did:
    // `tiff`, `tif`, `ico`, `bmp` and `avif` all encoded correctly while
    // the `ExportFormat` union still listed nine names, so TypeScript
    // rejected calls the library handles.
    let declared = fs
      .readFileSync("lib/index.d.ts", "utf8")
      .split("export type ExportFormat =")[1]
      .split(";")[0]
      .match(/"([a-z0-9]+)"/g)
      .map((quoted) => quoted.replace(/"/g, ""));

    let real = DESCRIBED.flatMap((format) => [format.name, ...format.aliases]);

    assert.deepStrictEqual(
      [...new Set(declared)].sort(),
      [...new Set(real)].sort(),
      "ExportFormat and the addon's format table must name the same formats",
    );
  });

  test("accepts every name the addon reports, and reports its media type", () => {
    let canvas = new Canvas(4, 4);
    canvas.getContext("2d");

    for (let { name, mime, aliases } of DESCRIBED) {
      for (let alias of [name, ...aliases, name.toUpperCase()]) {
        assert.ok(
          canvas.toURLSync(alias).startsWith(`data:${mime};base64,`),
          `${alias} should encode as ${mime}`,
        );
      }
    }
  });

  test("infers a format from a filename only where the table allows it", () => {
    let canvas = new Canvas(4, 4);
    canvas.getContext("2d");

    for (let { extension, inferable } of DESCRIBED) {
      let write = () => canvas.toFileSync(`${TMP}/inferred.${extension}`);
      if (inferable) assert.doesNotThrow(write, extension);
      // `raw` is the one that is not: a `.bin` file says nothing about its
      // pixel layout, so guessing one would write bytes nothing can read.
      else assert.throws(write, /Unsupported file format/, extension);
    }
  });

  test("lists the accepted names when given one it does not know", () => {
    let canvas = new Canvas(4, 4);
    canvas.getContext("2d");

    assert.throws(
      () => canvas.toBufferSync("targa"),
      (error) => {
        // Every name, from the table rather than from a sentence someone
        // has to remember to update.
        for (let { name } of DESCRIBED) {
          assert.match(error.message, new RegExp(`"${name}"`));
        }
        return /Unsupported file format "targa"/.test(error.message);
      },
    );
  });

  test("refuses timing given to a format that has no clock", () => {
    // Spanning pages and carrying timing are different questions, and TIFF,
    // ICO and PDF answer them differently: all three gather every page and
    // none has a frame rate. Asking any of them -- or any single-page
    // format -- to animate used to encode silently and say nothing, which
    // is the same silent retiming refused everywhere else here.
    let canvas = pages();

    for (let { name, animated } of DESCRIBED) {
      if (animated) continue;
      for (let [option, value] of [
        ["fps", 12],
        ["frameDelays", [100, 200, 350]],
        ["loop", 2],
      ]) {
        assert.throws(
          () => canvas.toBufferSync(name, { [option]: value }),
          new RegExp(`"${name}" is not an animated format`),
          `${name} + ${option}`,
        );
      }
    }
  });

  test("still encodes every format when no timing is named", () => {
    // The other half of the check above: leaving `fps` undefined must not
    // look like asking for it. The addon supplies the default, so the
    // binding sends nothing rather than sending 30.
    let canvas = pages();
    for (let { name } of DESCRIBED) {
      assert.ok(canvas.toBufferSync(name).length > 0, name);
    }
  });

  test("gathers every page only for a format that spans them", () => {
    // The one behaviour a hand-written `format == "pdf"` would get wrong the
    // moment a multi-page raster format is added: it would encode the last
    // page alone and report nothing amiss.
    let canvas = new Canvas(4, 4);
    canvas.getContext("2d");
    canvas.newPage();
    canvas.newPage();
    assert.equal(canvas.pages.length, 3);

    let spanning = DESCRIBED.filter((f) => f.spansPages);
    assert.ok(spanning.length > 0, "at least one format spans pages");

    for (let { name } of spanning) {
      let all = canvas.toBufferSync(name),
        one = canvas.toBufferSync(name, { page: 1 });
      assert.ok(
        all.length > one.length,
        `${name} of three pages should be larger than one of them`,
      );
    }

    for (let { name } of DESCRIBED.filter((f) => !f.spansPages)) {
      assert.deepEqual(
        canvas.toBufferSync(name),
        canvas.toBufferSync(name, { page: 3 }),
        `${name} should encode the current page and no other`,
      );
    }
  });
});

// GIF and APNG are the first formats this crate encodes itself: Skia has no
// encoder for either. Both gather every page into one animation, which is the
// combination -- raster and multi-page -- that no format here could express
// until the format table stopped treating those as one question.
describe("animated export", () => {
  let TMP;
  beforeEach(() => {
    TMP = tmp.dirSync().name;
  });

  /// A canvas of `colors.length` pages, each a solid two-by-one fill.
  const painted = (colors) => {
    let canvas = new Canvas(2, 1);
    for (let [index, color] of colors.entries()) {
      let ctx = index ? canvas.newPage() : canvas.getContext("2d");
      ctx.fillStyle = color;
      ctx.fillRect(0, 0, 2, 1);
    }
    return canvas;
  };

  const PRIMARIES = ["red", "lime", "blue"];
  const RGB = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
  ];

  test("writes one frame per page", async () => {
    let bytes = painted(PRIMARIES).toBufferSync("gif", {
      frameDelays: [100, 200, 350],
    });
    assert.deepEqual([...bytes.subarray(0, 6)], [...Buffer.from("GIF89a")]);

    // Read back through Skia, which decodes GIF even though it cannot write
    // one -- so this checks the file against a decoder that is not the
    // encoder's own.
    let img = await loadImage(bytes);
    assert.equal(img.frames, 3);
    assert.deepEqual(img.delays, [100, 200, 350]);
    assert.equal(img.width, 2);

    for (let [index, expected] of RGB.entries()) {
      let frame = img.frame(index),
        surface = new Canvas(frame.width, frame.height);
      surface.getContext("2d").drawImage(frame, 0, 0);
      assert.deepEqual(
        [...surface.toBufferSync("raw")],
        [...expected, ...expected],
        `frame ${index}`,
      );
    }
  });

  test("times frames by fps when no delays are given", async () => {
    let img = await loadImage(
      painted(PRIMARIES).toBufferSync("gif", {
        fps: 10,
      }),
    );
    assert.deepEqual(img.delays, [100, 100, 100]);
  });

  test("writes an APNG that is a PNG", () => {
    let bytes = painted(PRIMARIES).toBufferSync("apng");
    assert.deepEqual(
      [...bytes.subarray(0, 8)],
      [137, 80, 78, 71, 13, 10, 26, 10],
    );
    // `acTL` is what makes a PNG animated, and Skia's decoder ignores it --
    // which is why the frame-by-frame check lives in the Rust suite, against
    // the crate that wrote it.
    assert.ok(bytes.includes(Buffer.from("acTL")), "carries an animation");
  });

  test("infers both formats from a filename", () => {
    for (let extension of ["gif", "apng"]) {
      let file = `${TMP}/animated.${extension}`;
      painted(PRIMARIES).toFileSync(file);
      assert.ok(fs.statSync(file).size > 0, extension);
    }
  });

  test("refuses a frame-delay list with a hole or a non-number in it", () => {
    // `some`, `forEach` and every other iteration method skip a sparse
    // array's holes, so a list built by assigning into `new Array(n)` used
    // to pass both guards and reach the addon as `undefined`s -- which were
    // read as zero-length frames. The animation was retimed to nothing and
    // nothing said so.
    let sparse = new Array(3);
    sparse[0] = 1000;

    for (let bad of [
      sparse,
      [100, "x", 300],
      [100, NaN, 300],
      [100, Infinity, 300],
      [100, -5, 300],
      [100, undefined, 300],
      [100, null, 300],
    ]) {
      assert.throws(
        () => painted(PRIMARIES).toBufferSync("gif", { frameDelays: bad }),
        /array of non-negative numbers/,
        JSON.stringify(bad),
      );
    }
  });

  test("refuses a bad frame delay at the addon boundary too", () => {
    // The check above is in JavaScript, so it is only the first line. This
    // reaches past it to the addon, which used to default anything that was
    // not a number to zero rather than refusing it.
    let canvas = painted(PRIMARIES),
      pages = canvas.pages.map(core),
      base = {
        format: "gif",
        quality: 0.92,
        density: 1,
        outline: true,
        textContrast: 0,
        textGamma: 1.4,
        downsample: false,
        fps: 30,
        loop: 0,
      };

    let sparse = new Array(3);
    sparse[0] = 1000;

    assert.throws(
      () =>
        skiaNode.Canvas_toBufferSync(core(canvas), pages, {
          ...base,
          frameDelays: sparse,
        }),
      /number for .frameDelays\[1\]./,
    );
    assert.throws(
      () =>
        skiaNode.Canvas_toBufferSync(core(canvas), pages, {
          ...base,
          frameDelays: [100, -5, 300],
        }),
      /non-negative number for .frameDelays\[1\]. \(got -5\)/,
    );
    // And the valid case still gets through the same door.
    assert.ok(
      skiaNode.Canvas_toBufferSync(core(canvas), pages, {
        ...base,
        frameDelays: [100, 200, 350],
      }).length > 0,
    );
  });

  test("refuses a frame-delay list that does not match the pages", () => {
    // Ignoring it would retime the animation without saying so, which reads
    // as the argument doing nothing at all.
    assert.throws(
      () => painted(PRIMARIES).toBufferSync("gif", { frameDelays: [100] }),
      /one entry in .frameDelays. per page \(got 1 for 3\)/,
    );
    assert.throws(
      () => painted(PRIMARIES).toBufferSync("gif", { fps: 0 }),
      /positive number for .fps./,
    );
    assert.throws(
      () => painted(PRIMARIES).toBufferSync("gif", { loop: -1 }),
      /non-negative integer for .loop./,
    );
  });

  test("encodes a single page as one still frame", async () => {
    let img = await loadImage(painted(["red"]).toBufferSync("gif"));
    assert.equal(img.frames, 1);
  });
});

describe("colorType", () => {
  // The canvas's own pixel format is the default for everything read out of it.
  // Byte-per-pixel counts make it observable: Gray8 is 1, RGBA8888 and RGB888x are 4.
  const bytes = (canvas) => canvas.toBufferSync("raw").length;
  const filled = (opts) => {
    let canvas = new Canvas(10, 10, opts);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "rgba(255,0,0,0.5)";
    ctx.fillRect(0, 0, 10, 10);
    return { canvas, ctx };
  };

  test("is reported by the canvas", () => {
    assert.equal(new Canvas(1, 1).colorType, "rgba");
    assert.equal(new Canvas(1, 1, { colorType: "Gray8" }).colorType, "Gray8");
    assert.equal(new Canvas(1, 1, { colorType: "rgb" }).colorType, "rgb");
  });

  test("is inherited by toBuffer and getImageData", () => {
    let plain = filled();
    assert.equal(bytes(plain.canvas), 400);
    assert.equal(plain.ctx.getImageData(0, 0, 10, 10).data.length, 400);

    let gray = filled({ colorType: "Gray8" });
    assert.equal(bytes(gray.canvas), 100, "raw export");
    assert.equal(
      gray.ctx.getImageData(0, 0, 10, 10).data.length,
      100,
      "getImageData",
    );
  });

  test("is overridden by an explicit option on the call", () => {
    let { canvas, ctx } = filled({ colorType: "Gray8" });
    assert.equal(canvas.toBufferSync("raw", { colorType: "rgba" }).length, 400);
    assert.equal(
      ctx.getImageData(0, 0, 10, 10, { colorType: "rgba" }).data.length,
      400,
    );
  });

  test("distinguishes rgb from rgba by the padding byte", () => {
    // Both are 4 bytes wide, so only the last byte tells them apart: RGB888x pads
    // with 255 where RGBA8888 carries the real alpha.
    assert.deepEqual(
      Array.from(filled().canvas.toBufferSync("raw").subarray(0, 4)),
      [255, 0, 0, 128],
    );
    assert.deepEqual(
      Array.from(
        filled({ colorType: "rgb" }).canvas.toBufferSync("raw").subarray(0, 4),
      ),
      [255, 0, 0, 255],
    );
  });

  // Window's forwarding is deliberately not tested here: constructing a Window opens
  // a real OS window and keeps the GUI event loop alive, which hangs `node --test`.
  // The Canvas-level cases above cover the behaviour the forwarding depends on.
});
