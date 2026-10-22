const url = require("url"),
  { http, https } = require("follow-redirects"),
  { HttpsProxyAgent } = require("https-proxy-agent"),
  { skiaNode } = require("./classes/neon");

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

// The most redirects a load follows: the Fetch Standard's "if request's
// redirect count is 20, then return a network error". follow-redirects
// allows 21 unless told otherwise. The Rust crate's loader stops at 20 too.
const MAX_REDIRECTS = 20;

// Why a redirect that `redirect` says not to follow ends the load. The load
// rejects with a TypeError carrying this.
//
// "error" is a refusal in `fetch` too, and a TypeError there: Chrome 152
// rejects with "Failed to fetch" and Node 26 with "fetch failed", and neither
// requests the target. "manual" is not a refusal there: `fetch` resolves with
// the redirect itself -- Node's with the 3xx response and its `Location`,
// Chrome's with an opaque redirect. A redirect has no image in it, and a load
// resolves only with an image, so resolving here would mean resolving with
// nothing. It rejects instead, naming the status and where the redirect leads,
// which is what "manual" exists to hand the caller. It is a TypeError as well:
// `fetch` rejects with one whenever it has nothing it can return, and one type
// lets a caller catch "a redirect stopped this load" the same way whichever
// mode stopped it. Neither requests the target.
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

// `RequestInit.integrity` as the string `fetch` reads it. WebIDL converts a
// DOMString member with ToString, so a number or an object becomes its string
// and only a symbol cannot -- AGENTS.md's rule 5, a value of the wrong kind,
// and the TypeError `fetch` raises for one, before any request is made.
const integrityMetadata = (value) => {
  if (value === undefined) return "";
  if (typeof value == "symbol")
    throw TypeError("integrity must be a string (got: a symbol)");
  return String(value);
};

// Why `bytes` fail the integrity `metadata`, or null when they pass. The
// check is the crate's `Integrity` -- its parsing rules, and where they part
// from Chrome and from the specification, are written on `Integrity::parse`
// -- so the npm package and the Rust crate answer every case alike, and
// tests/assets/sri-cases.json holds both to it. Metadata naming no algorithm
// either knows -- an empty string, whitespace, only "md5-..." -- checks
// nothing, as in `fetch`.
//
// A failure is a TypeError because that is what `fetch` raises for one: a
// network error. `where` names the source in the message.
const integrityRefusal = (bytes, metadata, where) => {
  let refusal = skiaNode.integrityRefusal(bytes, metadata);
  return refusal == null
    ? null
    : TypeError(`Integrity check failed for ${where}: ${refusal}`);
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
    // A `maxRedirects` the caller passed through is follow-redirects' own
    // option rather than `RequestInit`'s, and is left as given.
    if (opts.maxRedirects === undefined) opts.maxRedirects = MAX_REDIRECTS;

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
        fail(
          TypeError(redirectRefused(url, status, headers.location, redirect)),
        );
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

// Whether `src` is a `data:` URL, by the rule the decoder reads one with: the
// scheme in any case, after any C0 controls and spaces, as `fetch` has it.
// A loader that tested less loosely would read `DATA:...` as a file path.
const isDataURL = (src) => {
  let at = 0;
  while (at < src.length && src.charCodeAt(at) <= 0x20) at++;
  return src.slice(at, at + 5).toLowerCase() == "data:";
};

// Hands the bytes of `dataURL` to `ok`, or a TypeError to `fail`. The decoding
// is the crate's `decode_data_url`, which reads a `data:` URL as Chrome's
// `fetch` does -- its rules are written there -- so npm and the Rust crate
// decode every case in tests/assets/data-url-cases.json alike.
const decodeDataURL = (dataURL, ok, fail) => {
  if (typeof dataURL != "string")
    return fail(
      TypeError(`Expected a data URL string (got ${typeof dataURL})`),
    );
  let decoded = skiaNode.decodeDataURL(dataURL);
  if (typeof decoded == "string")
    return fail(
      TypeError(
        `Expected a valid data URL string (got: "${dataURL}"): ${decoded}`,
      ),
    );
  // A throw from `ok` -- a decoder refusing the bytes -- is `fail`'s, as it
  // was when the decoding happened here.
  try {
    ok(decoded);
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

module.exports = {
  fetchURL,
  decodeDataURL,
  isDataURL,
  expandURL,
  integrityMetadata,
  integrityRefusal,
};
