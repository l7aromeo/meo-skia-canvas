// @ts-check

"use strict";

// What reaches a server when `loadImage` and `loadImageData` fetch a URL, and
// what a load does when it cannot use what came back.
//
// This file loads no nock. With nock installed, a request it passes through
// to a real server arrives with none of its headers, so a test of headers
// beside nock reads nock's behaviour rather than this library's.

const fs = require("fs"),
  http = require("http"),
  { execFile } = require("child_process"),
  { assert, describe, test } = require("../runner"),
  { loadImage, loadImageData } = require("../../lib");

const PATH = "tests/assets/images/pentagon.png",
  BUFFER = fs.readFileSync(PATH);

// `127.0.0.1` and `localhost` are two hosts to follow-redirects, so a
// redirect from one to the other is the cross-host case. It strips
// `Authorization`, `Cookie` and `Proxy-Authorization` there, matching by
// name, so a credential carried under any other name goes through. Real
// servers rather than nock, so the redirect is followed by the same code a
// caller's request is.
//
// Loads from `127.0.0.1`, which redirects to `localhost`, which serves
// `body`. Returns what each host received and what the load threw, if
// anything; a host that was never reached is left undefined.
const throughRedirect = async (load, body = BUFFER) => {
  /** @type {{ first?: http.IncomingHttpHeaders, second?: http.IncomingHttpHeaders, error?: any }} */
  let seen = {},
    second = http.createServer((req, res) => {
      seen.second = req.headers;
      res.end(body);
    }),
    first = http.createServer((req, res) => {
      seen.first = req.headers;
      let { port } = /** @type {import("net").AddressInfo} */ (
        second.address()
      );
      res.writeHead(302, { Location: `http://localhost:${port}/${PATH}` });
      res.end();
    });
  await new Promise((ok) => second.listen(0, "localhost", () => ok(0)));
  await new Promise((ok) => first.listen(0, "127.0.0.1", () => ok(0)));
  let { port } = /** @type {import("net").AddressInfo} */ (first.address());
  try {
    await load(`http://127.0.0.1:${port}/${PATH}`);
  } catch (error) {
    seen.error = error;
  } finally {
    for (let server of [first, second]) {
      server.closeAllConnections();
      server.close();
    }
  }
  return seen;
};

describe("A remote image is fetched with the request headers it is given", () => {
  // One credential in each shape `RequestInit.headers` takes.
  const SHAPES = {
    "a plain object": () => ({ Authorization: "Bearer A" }),
    "an array of pairs": () => [["Authorization", "Bearer A"]],
    "a Headers instance": () => new Headers({ Authorization: "Bearer A" }),
  };

  for (let [shape, headers] of Object.entries(SHAPES)) {
    test(`given as ${shape}, to the first host and no further`, async () => {
      let { first, second, error } = await throughRedirect((url) =>
        loadImage(url, { headers: headers() }),
      );
      // One comparison, so a failure shows every half at once. The second
      // host is searched by value rather than by name: the credential must be
      // absent from every header it saw, whatever it was sent as.
      assert.deepEqual(
        {
          error,
          atFirstHost: first?.authorization,
          atSecondHost: Object.entries(second ?? {}).filter(([, value]) =>
            String(value).includes("Bearer A"),
          ),
        },
        { error: undefined, atFirstHost: "Bearer A", atSecondHost: [] },
      );
    });
  }

  for (let [shape, headers] of Object.entries({
    "a plain object": () => ({ "User-Agent": "mine/1" }),
    "an array of pairs": () => [["User-Agent", "mine/1"]],
    "a Headers instance": () => new Headers({ "User-Agent": "mine/1" }),
  })) {
    test(`a User-Agent given as ${shape} replaces the default`, async () => {
      let { first } = await throughRedirect((url) =>
        loadImage(url, { headers: headers() }),
      );
      assert.equal(first?.["user-agent"], "mine/1");
    });
  }

  test("no headers, or a falsy value for them, sends the default User-Agent", async () => {
    let given = [undefined, null, false, ""],
      outcomes = [];
    for (let headers of given) {
      let { first, error } = await throughRedirect((url) =>
        loadImage(url, /** @type {any} */ ({ headers })),
      );
      outcomes.push({ headers, error, userAgent: first?.["user-agent"] });
    }
    assert.deepEqual(
      outcomes,
      given.map((headers) => ({
        headers,
        error: undefined,
        userAgent: "Skia Canvas",
      })),
    );
  });

  test("loadImageData sends them too", async () => {
    let { first, second, error } = await throughRedirect(
      (url) =>
        loadImageData(url, 2, 2, { headers: [["Authorization", "Bearer A"]] }),
      Buffer.alloc(2 * 2 * 4),
    );
    assert.deepEqual(
      {
        error,
        atFirstHost: first?.authorization,
        atSecondHost: second?.authorization,
      },
      { error: undefined, atFirstHost: "Bearer A", atSecondHost: undefined },
    );
  });

  test("refuses a value that is not a list of headers, before any request", async () => {
    let given = [
        "Authorization: Bearer A",
        42,
        [["Authorization"]],
        [["Authorization", "Bearer", "A"]],
        { "not a name": "x" },
      ],
      outcomes = [];
    for (let headers of given) {
      let { first, error } = await throughRedirect((url) =>
        loadImage(url, /** @type {any} */ ({ headers })),
      );
      outcomes.push({
        headers,
        refusedAs: error?.name,
        namesHeaders: /headers/i.test(error?.message ?? ""),
        reachedServer: first !== undefined,
      });
    }
    assert.deepEqual(
      outcomes,
      given.map((headers) => ({
        headers,
        refusedAs: "TypeError",
        namesHeaders: true,
        reachedServer: false,
      })),
    );
  });
});

// `RequestInit.redirect` decides whether the second host is asked at all, so
// each case records whether it was. A load that refuses a redirect has to
// refuse it before the target is requested: refusing afterwards would already
// have sent whatever the request carried there.
describe("A redirect is followed only as `redirect` allows", () => {
  // What a load through the redirect came to, in one comparable shape.
  const outcome = async (load, body) => {
    let { first, second, error } = await throughRedirect(load, body);
    return {
      askedFirst: first !== undefined,
      askedSecond: second !== undefined,
      error: error && `${error.name}: ${error.message}`,
    };
  };

  for (let redirect of [undefined, "follow"]) {
    test(`${redirect ?? "unset"}: the redirect is followed`, async () => {
      assert.deepEqual(await outcome((url) => loadImage(url, { redirect })), {
        askedFirst: true,
        askedSecond: true,
        error: undefined,
      });
    });
  }

  test('"error": the load rejects, naming the status, and the target is never asked', async () => {
    let { askedFirst, askedSecond, error } = await outcome((url) =>
      loadImage(url, { redirect: "error" }),
    );
    assert.deepEqual(
      {
        askedFirst,
        askedSecond,
        refusedAs: error?.split(":")[0],
        namesStatus: /\b302\b/.test(error ?? ""),
        saysRefused: /refused/i.test(error ?? ""),
      },
      {
        askedFirst: true,
        askedSecond: false,
        refusedAs: "TypeError",
        namesStatus: true,
        saysRefused: true,
      },
    );
  });

  test('"manual": the load rejects with where the redirect leads, and the target is never asked', async () => {
    let { askedFirst, askedSecond, error } = await outcome((url) =>
      loadImage(url, { redirect: "manual" }),
    );
    assert.deepEqual(
      {
        askedFirst,
        askedSecond,
        refusedAs: error?.split(":")[0],
        namesStatus: /\b302\b/.test(error ?? ""),
        namesTarget: /http:\/\/localhost:\d+\//.test(error ?? ""),
      },
      {
        askedFirst: true,
        askedSecond: false,
        refusedAs: "TypeError",
        namesStatus: true,
        namesTarget: true,
      },
    );
  });

  test("loadImageData honours it too", async () => {
    // Raw pixels, which is what `loadImageData` reads, so a load that follows
    // the redirect resolves rather than failing on the bytes it got.
    let { askedSecond, error } = await outcome(
      (url) => loadImageData(url, 2, 2, { redirect: "error" }),
      Buffer.alloc(2 * 2 * 4),
    );
    assert.deepEqual(
      { askedSecond, refusedAs: error?.split(":")[0] },
      { askedSecond: false, refusedAs: "TypeError" },
    );
  });

  test("a value that is not a redirect mode is a TypeError, before any request", async () => {
    let given = ["bogus", "Follow", null, 1],
      outcomes = [];
    for (let redirect of given) {
      let { askedFirst, error } = await outcome((url) =>
        loadImage(url, /** @type {any} */ ({ redirect })),
      );
      outcomes.push({
        redirect,
        refusedAs: error?.split(":")[0],
        namesRedirect: /redirect/.test(error ?? ""),
        askedFirst,
      });
    }
    assert.deepEqual(
      outcomes,
      given.map((redirect) => ({
        redirect,
        refusedAs: "TypeError",
        namesRedirect: true,
        askedFirst: false,
      })),
    );
  });
});

// What a load does when it cannot turn the bytes it fetched into an image.
//
// The defect being guarded escaped to the top level -- the promise never
// settled and the error surfaced as an uncaught exception -- so each load
// runs in a child process that installs no handler for one. An escape there
// ends the child, which is exactly the outcome under test, and this process
// reads the exit status instead of being ended itself: node:test's own
// handler would otherwise attribute the exception to whichever test was
// running, or the runner would exit. A load that never settles is bounded by
// a timer inside the child, so a regression fails rather than hangs.
//
// Asynchronous `execFile`, not `execFileSync`: the server answering the
// child runs in this process, and a blocked event loop would never reply.
const runLoad = (which, url = "") => {
  let script = `
    const { loadImage, loadImageData } = require(${JSON.stringify(require.resolve("../../lib"))});
    const load = {
      image: () => loadImage(${JSON.stringify(url)}),
      data: () => loadImageData(${JSON.stringify(url)}, 2, 2),
      sharp: () =>
        loadImageData(
          require(${JSON.stringify(require.resolve("sharp"))})({
            create: { width: 2, height: 2, channels: 4, background: "#f00" },
          }).png(),
          2, 2, { colorSpace: "not-a-space" },
        ),
    }[${JSON.stringify(which)}];
    Promise.race([
      load().then(
        () => ({ state: "resolved" }),
        (e) => ({ state: "rejected", name: e.name, message: e.message }),
      ),
      new Promise((done) => setTimeout(() => done({ state: "pending" }), 2000)),
    ]).then((outcome) => {
      console.log(JSON.stringify(outcome));
      process.exit(0);
    });`;
  return new Promise((done) =>
    execFile(process.execPath, ["-e", script], (error, stdout, stderr) =>
      done({
        exitCode: error ? (error.code ?? 1) : 0,
        outcome: stdout.trim() ? JSON.parse(stdout.trim()) : undefined,
        stderr: stderr.trim().split("\n").slice(0, 3).join(" | "),
      }),
    ),
  );
};

// A server answering every request with `body`, for as long as `run` takes.
const serving = async (body, run, status = 200) => {
  let server = http.createServer((req, res) => {
    res.writeHead(status);
    res.end(body);
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", () => ok(0)));
  let { port } = /** @type {import("net").AddressInfo} */ (server.address());
  try {
    return await run(`http://127.0.0.1:${port}/${PATH}`);
  } finally {
    server.closeAllConnections();
    server.close();
  }
};

describe("A load that cannot use what it fetched rejects, and nothing escapes", () => {
  test("a body that does not fit loadImageData's size rejects as the same bytes in a Buffer do", async () => {
    // The control: the Buffer path never had the defect, because a Buffer is
    // handed over synchronously inside the promise. Ten bytes is not a whole
    // number of RGBA pixels.
    let control = await loadImageData(Buffer.alloc(10), 2, 2).then(
      () => undefined,
      (e) => ({ state: "rejected", name: e.name, message: e.message }),
    );
    assert.equal(control?.name, "InvalidStateError");

    let { exitCode, outcome, stderr } = await serving(Buffer.alloc(10), (url) =>
      runLoad("data", url),
    );
    assert.equal(exitCode, 0, `the load ended its process: ${stderr}`);
    assert.deepEqual(outcome, control);
  });

  test("a body loadImage cannot decode rejects", async () => {
    let { exitCode, outcome, stderr } = await serving(
      Buffer.from("this is text, not an image"),
      (url) => runLoad("image", url),
    );
    assert.equal(exitCode, 0, `the load ended its process: ${stderr}`);
    assert.equal(outcome?.state, "rejected");
    assert.match(outcome?.message ?? "", /decode/);
  });

  test("a sharp source the constructor refuses rejects too", async () => {
    // Not a request at all, and the same escape: sharp hands its pixels over
    // from a `toBuffer` callback, outside the promise, just as a response
    // does from its `end` event.
    let { exitCode, outcome, stderr } = await runLoad("sharp");
    assert.equal(exitCode, 0, `the load ended its process: ${stderr}`);
    assert.deepEqual(
      { state: outcome?.state, name: outcome?.name },
      { state: "rejected", name: "TypeError" },
    );
  });
});

describe("A refused load releases its connection", () => {
  test("two refused loads in a row through an agent holding one socket both settle", async () => {
    // An unread response keeps its socket checked out of the agent, so with
    // one socket the second load waits for a release that never comes.
    let agent = new http.Agent({ keepAlive: true, maxSockets: 1 }),
      settle = (url) =>
        Promise.race([
          loadImage(url, { agent }).then(
            () => "resolved",
            () => "rejected",
          ),
          new Promise((done) => setTimeout(() => done("stalled"), 1500)),
        ]);
    try {
      let outcomes = await serving(
        "x".repeat(100000),
        async (url) => [await settle(url), await settle(url)],
        404,
      );
      assert.deepEqual(outcomes, ["rejected", "rejected"]);
    } finally {
      agent.destroy();
    }
  });
});

// `RequestInit.integrity` is checked against what arrived, so each case loads
// the fixture with some metadata and records whether the load went through.
//
// The digests are the fixture's, computed with `openssl dgst -sha<n> -binary
// tests/assets/images/pentagon.png | openssl base64 -A` rather than with the
// `crypto` module the library itself hashes with, so a wrong digest there
// cannot agree with a wrong digest here. sha512's carries both "+" and "/",
// which is what lets the base64url case differ from the base64 one.
describe("A fetched image is checked against `integrity`", () => {
  const DIGEST = {
    sha256: "Ql6jn9vVPKFySvGSeUJOsakZNmUmbEhintmrRgipg2Y=",
    sha384: "nDF6hI4wxRc0vgIfXx0q329AI3g481wCZNcvoeG2FWUxI1FQBlH44S/CEWszTFQq",
    sha512:
      "CrROHCuvh9ZrUYt2Wf5lXJEbYSI2F6YtN4V/Gi39O4X8lgMZkL5c/RoIZn353HPx+2I5A8iOi5AIyIjRF6FRlA==",
  };
  // Right length and alphabet, wrong value.
  const WRONG = {
    sha256: `${"A".repeat(43)}=`,
    sha512: `${"A".repeat(86)}==`,
  };
  const base64url = (digest) => digest.replace(/\+/g, "-").replace(/\//g, "_");
  // 2 x 2 RGBA of zeros; its sha256, from `head -c 16 /dev/zero | openssl
  // dgst -sha256 -binary | openssl base64 -A`.
  const ZEROS_SHA256 = "N0cI//dxndWXnsh11WzSKG9tPPfsMXo7JWMqqyjsN7s=";

  // How a load with `integrity` came out: "load", or the rejection's name and
  // whether its message is about integrity.
  const outcome = (integrity) =>
    serving(BUFFER, (url) =>
      loadImage(url, /** @type {any} */ ({ integrity })).then(
        () => "load",
        (e) => `${e.name}${/integrity/i.test(e.message) ? " (integrity)" : ""}`,
      ),
    );
  const REFUSED = "TypeError (integrity)";

  // Each row is the metadata and what a load with it does. One comparison per
  // group, so a failure shows every row that disagrees at once.
  const check = async (rows) => {
    let got = [];
    for (let [integrity, expected] of rows)
      got.push([integrity, await outcome(integrity), expected]);
    assert.deepEqual(
      got.map(([integrity, actual]) => [integrity, actual]),
      got.map(([integrity, , expected]) => [integrity, expected]),
    );
  };

  test("loads when a hash of the strongest algorithm given matches, or none can be checked", async () => {
    await check([
      [`sha256-${DIGEST.sha256}`, "load"],
      [`sha384-${DIGEST.sha384}`, "load"],
      [`sha512-${DIGEST.sha512}`, "load"],
      // Nothing checkable: no metadata, whitespace, an algorithm SRI does not
      // define, and a number, which is read as its string.
      ["", "load"],
      [" \t\n ", "load"],
      ["md5-AAAA", "load"],
      [1, "load"],
      // An unknown algorithm beside a known one is skipped.
      [`md5-AAAA sha256-${DIGEST.sha256}`, "load"],
      // Only the strongest algorithm given counts, and any of its hashes will
      // do.
      [`sha256-${WRONG.sha256} sha512-${DIGEST.sha512}`, "load"],
      [`sha256-${WRONG.sha256} sha256-${DIGEST.sha256}`, "load"],
      // Any ASCII whitespace separates, as in Chrome.
      [`sha256-${WRONG.sha256}\nsha512-${DIGEST.sha512}`, "load"],
      [`sha256-${DIGEST.sha256}\tmd5-x`, "load"],
      // base64url and missing padding read as the digest they spell.
      [`sha512-${base64url(DIGEST.sha512)}`, "load"],
      [`sha512-${DIGEST.sha512.replace(/=+$/, "")}`, "load"],
      // An option suffix is ignored.
      [`sha256-${DIGEST.sha256}?opt`, "load"],
    ]);
  });

  test("refuses, with a TypeError about integrity, when none of them matches", async () => {
    await check([
      [`sha256-${WRONG.sha256}`, REFUSED],
      [`md5-AAAA sha256-${WRONG.sha256}`, REFUSED],
      // The strongest algorithm given is the one checked, even beside a
      // weaker hash that matches.
      [`sha256-${DIGEST.sha256} sha512-${WRONG.sha512}`, REFUSED],
      // The value is compared exactly.
      [`sha256-${DIGEST.sha256.toLowerCase()}`, REFUSED],
      // The three places Subresource Integrity's text, followed here, refuses
      // where Chrome 152 loads unchecked: an algorithm in upper case is still
      // one it knows, and a known algorithm with no value can never match.
      [`SHA512-${WRONG.sha512} sha256-${DIGEST.sha256}`, REFUSED],
      ["sha256-", REFUSED],
      ["sha256", REFUSED],
    ]);
  });

  test("a symbol is a TypeError, before any request", async () => {
    let { first, error } = await throughRedirect((url) =>
      loadImage(url, /** @type {any} */ ({ integrity: Symbol("x") })),
    );
    assert.deepEqual(
      {
        refusedAs: error?.name,
        namesIntegrity: /integrity/.test(error?.message ?? ""),
        askedServer: first !== undefined,
      },
      { refusedAs: "TypeError", namesIntegrity: true, askedServer: false },
    );
  });

  test("the body checked is the one a redirect leads to", async () => {
    let { second, error } = await throughRedirect((url) =>
      loadImage(url, { integrity: `sha256-${WRONG.sha256}` }),
    );
    assert.deepEqual(
      { followed: second !== undefined, refusedAs: error?.name },
      { followed: true, refusedAs: "TypeError" },
    );
  });

  test("loadImageData checks it too", async () => {
    let load = (integrity) =>
      serving(Buffer.alloc(16), (url) =>
        loadImageData(url, 2, 2, { integrity }).then(
          () => "load",
          (e) => e.name,
        ),
      );
    assert.deepEqual(
      {
        matching: await load(`sha256-${ZEROS_SHA256}`),
        mismatching: await load(`sha256-${WRONG.sha256}`),
      },
      { matching: "load", mismatching: "TypeError" },
    );
  });

  test("a data: URL is checked too, as fetch checks one", async () => {
    let url = `data:image/png;base64,${BUFFER.toString("base64")}`,
      load = (integrity) =>
        loadImage(url, { integrity }).then(
          () => "load",
          (e) => e.name,
        );
    assert.deepEqual(
      {
        matching: await load(`sha256-${DIGEST.sha256}`),
        mismatching: await load(`sha256-${WRONG.sha256}`),
      },
      { matching: "load", mismatching: "TypeError" },
    );
  });
  // Every source the loaders take is checked, not only a URL: the bytes are
  // in hand for each. `settled` turns a load into "load" or the rejection's
  // name, so match and mismatch compare in one assertion.
  const settled = (promise) =>
    promise.then(
      () => "load",
      (e) => `${e.name}${/integrity/i.test(e.message) ? " (integrity)" : ""}`,
    );
  const REFUSED_HERE = "TypeError (integrity)";

  test("a file read from disk is checked too", async () => {
    assert.deepEqual(
      {
        matching: await settled(
          loadImage(PATH, { integrity: `sha256-${DIGEST.sha256}` }),
        ),
        mismatching: await settled(
          loadImage(PATH, { integrity: `sha256-${WRONG.sha256}` }),
        ),
      },
      { matching: "load", mismatching: REFUSED_HERE },
    );
  });

  test("a Buffer is checked too, by both loaders", async () => {
    let zeros = Buffer.alloc(16);
    assert.deepEqual(
      {
        image: await settled(
          loadImage(
            BUFFER,
            /** @type {any} */ ({ integrity: `sha256-${DIGEST.sha256}` }),
          ),
        ),
        imageWrong: await settled(
          loadImage(
            BUFFER,
            /** @type {any} */ ({ integrity: `sha256-${WRONG.sha256}` }),
          ),
        ),
        data: await settled(
          loadImageData(zeros, 2, 2, { integrity: `sha256-${ZEROS_SHA256}` }),
        ),
        dataWrong: await settled(
          loadImageData(zeros, 2, 2, { integrity: `sha256-${WRONG.sha256}` }),
        ),
      },
      {
        image: "load",
        imageWrong: REFUSED_HERE,
        data: "load",
        dataWrong: REFUSED_HERE,
      },
    );
  });

  test("a sharp image is checked against the bytes it decodes", async () => {
    const sharp = require("sharp"),
      { Readable } = require("stream"),
      load = (image, integrity) =>
        settled(
          loadImage(
            /** @type {any} */ (image),
            /** @type {any} */ ({ integrity }),
          ),
        );
    assert.deepEqual(
      {
        fromBuffer: await load(sharp(BUFFER), `sha256-${DIGEST.sha256}`),
        fromBufferWrong: await load(sharp(BUFFER), `sha256-${WRONG.sha256}`),
        // Raw pixels are the bytes themselves.
        fromRaw: await load(
          sharp(Buffer.alloc(16), {
            raw: { width: 2, height: 2, channels: 4 },
          }),
          `sha256-${ZEROS_SHA256}`,
        ),
        // A stream's bytes exist only once sharp has read them.
        fromStream: await load(
          Readable.from([BUFFER]).pipe(sharp()),
          `sha256-${DIGEST.sha256}`,
        ),
        fromStreamWrong: await load(
          Readable.from([BUFFER]).pipe(sharp()),
          `sha256-${WRONG.sha256}`,
        ),
      },
      {
        fromBuffer: "load",
        fromBufferWrong: REFUSED_HERE,
        fromRaw: "load",
        fromStream: "load",
        fromStreamWrong: REFUSED_HERE,
      },
    );
  });

  test("a sharp image with no input bytes in hand is refused when integrity is set", async () => {
    const sharp = require("sharp"),
      blank = () =>
        sharp({
          create: { width: 2, height: 2, channels: 4, background: "red" },
        }),
      reason = (image, integrity) =>
        loadImage(
          /** @type {any} */ (image),
          /** @type {any} */ ({ integrity }),
        ).then(
          () => "load",
          (e) =>
            `${e.name}: ${/sharp\(buffer\)/.test(e.message) ? "names the fix" : e.message}`,
        );
    // The control is the file: without `integrity` it loads, so the refusal is
    // about `integrity`. A blank sharp image has no control, because it does not
    // load at all yet -- with nothing to name it by, it has no `src`.
    assert.deepEqual(
      {
        fromFile: await reason(sharp(PATH), `sha256-${DIGEST.sha256}`),
        blank: await reason(blank(), `sha256-${DIGEST.sha256}`),
        fromFileUnset: await reason(sharp(PATH), undefined),
      },
      {
        fromFile: "TypeError: names the fix",
        blank: "TypeError: names the fix",
        fromFileUnset: "load",
      },
    );
  });
  test("bytes in any other byte-sized view are checked as a Buffer is", async () => {
    let view = () => new Uint8Array(BUFFER),
      memory = () =>
        BUFFER.buffer.slice(
          BUFFER.byteOffset,
          BUFFER.byteOffset + BUFFER.byteLength,
        ),
      load = (src, integrity) =>
        settled(
          loadImage(
            /** @type {any} */ (src),
            /** @type {any} */ ({ integrity }),
          ),
        );
    assert.deepEqual(
      {
        view: await load(view(), `sha256-${DIGEST.sha256}`),
        viewWrong: await load(view(), `sha256-${WRONG.sha256}`),
        memory: await load(memory(), `sha256-${DIGEST.sha256}`),
        memoryWrong: await load(memory(), `sha256-${WRONG.sha256}`),
      },
      {
        view: "load",
        viewWrong: REFUSED_HERE,
        memory: "load",
        memoryWrong: REFUSED_HERE,
      },
    );
  });
});
