// @ts-check

"use strict";

const fs = require("fs"),
  tmp = require("tmp"),
  path = require("path"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  { Canvas, Image, loadImage, backend } = require("../../lib");

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
        () => canvas.toFile(`${TMP}/output.gif`),
        /Unsupported file format/,
      );
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
