// @ts-check

"use strict";

// What reaches a server when `loadImage` and `loadImageData` fetch a URL.
//
// This file loads no nock. With nock installed, a request it passes through
// to a real server arrives with none of its headers, so a test of headers
// beside nock reads nock's behaviour rather than this library's.

const fs = require("fs"),
  http = require("http"),
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
        namesStatus: /\b302\b/.test(error ?? ""),
        saysRefused: /refused/i.test(error ?? ""),
      },
      {
        askedFirst: true,
        askedSecond: false,
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
        namesStatus: /\b302\b/.test(error ?? ""),
        namesTarget: /http:\/\/localhost:\d+\//.test(error ?? ""),
      },
      {
        askedFirst: true,
        askedSecond: false,
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
      { askedSecond, rejected: error !== undefined },
      { askedSecond: false, rejected: true },
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
