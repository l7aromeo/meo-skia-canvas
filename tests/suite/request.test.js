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
