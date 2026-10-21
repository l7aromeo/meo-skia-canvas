const url = require("url"),
  { http, https } = require("follow-redirects"),
  { HttpsProxyAgent } = require("https-proxy-agent");

const USER_AGENT = "Skia Canvas";
const PROXY_URL =
  process.env.https_proxy ||
  process.env.HTTPS_PROXY ||
  process.env.http_proxy ||
  process.env.HTTP_PROXY;

// The request headers as the plain record follow-redirects takes, with the
// default User-Agent added unless the caller set their own.
//
// `RequestInit` lets `headers` be a record, a list of name/value pairs or a
// `Headers`, and only a record can be spread into another. A spread list
// becomes headers named `0`, `1`, ... -- which follow-redirects does not
// recognise as a credential, so a cross-host redirect carries them on -- and a
// spread `Headers` becomes nothing at all. `Headers` reads every shape, so it
// is the one parser, and its names come out lowercased, which matches HTTP's
// case-insensitive names and the follow-redirects filter alike.
//
// A falsy `headers` means none, as spreading one always has: `cond && headers`
// is a reasonable thing to write. Anything else `Headers` cannot read throws
// its `TypeError` -- AGENTS.md's rule 3 for a pair of the wrong length, its
// rule 5 for a value of the wrong kind, and what `fetch` raises for both.
const requestHeaders = (init) => {
  let headers = new Headers(init || undefined);
  if (!headers.has("User-Agent")) headers.set("User-Agent", USER_AGENT);
  return Object.fromEntries(headers);
};

// The values `RequestInit.redirect` takes: the Fetch Standard's
// `RequestRedirect` enumeration, of which "follow" is the default.
const REDIRECT_MODES = ["follow", "error", "manual"];

// Why a redirect that `redirect` says not to follow ends the load.
//
// "error" is a refusal, as in `fetch`. "manual" is not one there: `fetch`
// resolves with the redirect itself -- Node's with the 3xx response and its
// `Location`, a browser's with an opaque redirect. A redirect has no image in
// it, and a load resolves only with an image, so resolving here would mean
// resolving with nothing. It rejects instead, naming the status and where the
// redirect leads, which is what "manual" exists to hand the caller. Neither
// requests the target.
const redirectRefused = (url, status, location, redirect) => {
  let target = location;
  try {
    target = new URL(location, url).href;
  } catch {
    // A `Location` that is not a URL is reported as the server sent it.
  }
  return redirect == "error"
    ? `Refused to follow a redirect from "${url}" to "${target}" ` +
        `(HTTP ${status}), as \`redirect\` is "error"`
    : `"${url}" redirects to "${target}" (HTTP ${status}), which ` +
        `\`redirect: "manual"\` leaves to the caller to follow`;
};

const fetchURL = (url, opts, ok, fail) => {
  let proto = url.slice(0, 5).split(":")[0],
    client = { http, https }[proto.toLowerCase()];

  if (!client) {
    fail(
      new Error(
        `Unsupported protocol: expected 'http' or 'https' (got: ${proto})`,
      ),
    );
  } else {
    opts = opts || {};
    try {
      opts.headers = requestHeaders(opts.headers);
    } catch (e) {
      return fail(
        TypeError(
          `Request headers must be an object, an array of [name, value] ` +
            `pairs or a Headers instance (${e.message})`,
        ),
      );
    }
    // follow-redirects has no `redirect` option and follows every redirect
    // unless `followRedirects` is false, when it hands back the 3xx response
    // without requesting where it points. A value outside the enumeration is
    // AGENTS.md's rule 2, a TypeError -- what `fetch` raises for one too, and
    // like it, before any request is made.
    let redirect = opts.redirect === undefined ? "follow" : opts.redirect;
    if (!REDIRECT_MODES.includes(redirect))
      return fail(
        TypeError(
          `redirect must be "follow", "error" or "manual" ` +
            `(got: ${String(redirect)})`,
        ),
      );
    if (redirect != "follow") opts.followRedirects = false;

    opts.agent =
      opts.agent === undefined && PROXY_URL
        ? new HttpsProxyAgent(PROXY_URL)
        : opts.agent;

    let req = client.request(url, opts, (resp) => {
      let { statusCode: status, headers } = resp;
      // Both refusals below want nothing in the response, and reading it to
      // the end is what returns the socket to its agent. Left unread, the
      // socket stays checked out, so an agent with a socket limit stalls the
      // next load to the same host once enough of them have been refused.
      if (
        redirect != "follow" &&
        headers.location &&
        status >= 300 &&
        status < 400
      ) {
        resp.resume();
        fail(Error(redirectRefused(url, status, headers.location, redirect)));
      } else if (status < 200 || status >= 300) {
        resp.resume();
        fail(
          new Error(
            `Failed to load image from "${url}" (HTTP error ${resp.statusCode})`,
          ),
        );
      } else {
        const chunks = [];
        resp.on("data", (chunk) => chunks.push(chunk));
        resp.on("end", () => ok(Buffer.concat(chunks)));
        resp.on("error", (e) => fail(e));
      }
    });

    req.on("error", (e) => fail(e));
    if (opts.body) req.write(opts.body);
    req.end();
  }
};

const decodeDataURL = (dataURL, ok, fail) => {
  if (typeof dataURL != "string")
    return fail(
      TypeError(`Expected a data URL string (got ${typeof dataURL})`),
    );
  // RFC 2397: `data:` [ mediatype ] [ ";base64" ] "," data -- everything
  // before the first comma is the media type and its parameters, and both the
  // media type and `;base64` are optional. So the comma is the only part that
  // has to be there, and the whole prefix is matched rather than a fixed
  // window of it: a media type can be longer than any window worth choosing,
  // and `application/vnd.oasis.opendocument.graphics` already is.
  let [header, meta] = dataURL.match(/^\s*data:([^,]*),/) || [];
  if (header === undefined)
    return fail(
      TypeError(`Expected a valid data URL string (got: "${dataURL}")`),
    );

  // The encoding is the `;base64` suffix and nothing else. A `charset`
  // parameter names the text's encoding, not the URL's, and reading one as the
  // other is what made `data:image/svg+xml,<svg ...>` -- the form CSS and
  // every "icon as a string" helper emits -- report itself as invalid.
  let isBase64 = /;base64\s*$/i.test(meta),
    content = dataURL.slice(header.length);

  try {
    ok(
      isBase64
        ? Buffer.from(content, "base64")
        : Buffer.from(decodeURIComponent(content), "utf8"),
    );
  } catch (e) {
    fail(e);
  }
};

const expandURL = (src) => {
  // convert URLs to strings, otherwise pass arg through unmodified
  if (src instanceof URL) {
    if (src.protocol == "file:") src = url.fileURLToPath(src);
    else if (src.protocol.match(/^(https?|data):/)) src = src.href;
    else throw Error(`Unsupported protocol: ${src.protocol.replace(":", "")}`);
  }
  return src;
};

module.exports = { fetchURL, decodeDataURL, expandURL };
