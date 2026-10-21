const url = require("url"),
  { createHash } = require("crypto"),
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

// The hash algorithms Subresource Integrity defines, weakest first: the
// specification's "valid SRI hash algorithm token set", whose order is what
// "strongest" means.
const SRI_ALGORITHMS = ["sha256", "sha384", "sha512"];

// U+0009 TAB, U+000A LF, U+000C FF, U+000D CR and U+0020 SPACE: Infra's
// "ASCII whitespace".
const ASCII_WHITESPACE = /[\t\n\f\r ]+/;

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

// The hashes in `metadata` this library can check: the Subresource
// Integrity specification's "parse metadata", except where its text, Chrome
// 152 and Node 26's `fetch` do not all agree. There, each rule below says
// which it follows and why.
//
// - Items are separated by any ASCII whitespace, as in Chrome. The
//   specification strictly splits on U+0020 alone, which Node 26's `fetch`
//   does; but its own examples put one hash per line, and a strict split
//   reads the line break as part of the first hash.
// - The value is everything after the first "-", as in Chrome and Node. The
//   specification splits on every "-" and keeps only the second part, which
//   cuts a base64url value -- a form its grammar admits -- short.
// - The algorithm is compared ASCII case-insensitively, as the specification
//   says, where Chrome and Node compare it exactly and so read "SHA512-..."
//   as an algorithm they do not know and skip the check.
//
// A recognised algorithm with no value is kept with an empty one, as the
// specification's steps say, and can never match. Chrome instead skips such
// an item, and an item whose value holds a character no encoding uses, and so
// loads bytes it never checked; this refuses them. An unknown algorithm is
// skipped, as everywhere: Subresource Integrity treats one as no metadata,
// so that a stronger hash added later does not break older readers.
const parseIntegrity = (metadata) =>
  metadata.split(ASCII_WHITESPACE).flatMap((item) => {
    // Anything after "?" is an option, and none are defined.
    let [expression] = item.split("?"),
      dash = expression.indexOf("-"),
      algorithm = (dash < 0 ? expression : expression.slice(0, dash)).replace(
        /[A-Z]/g,
        (c) => c.toLowerCase(),
      );
    if (!SRI_ALGORITHMS.includes(algorithm)) return [];
    return [{ algorithm, value: dash < 0 ? "" : expression.slice(dash + 1) }];
  });

// A digest as it is compared: base64url's "-" and "_" read as base64's "+"
// and "/", and padding dropped. The specification compares exact base64, but
// its grammar admits both encodings -- CSP, which defines that grammar,
// treats them as equivalent -- and Chrome and Node accept either, with or
// without padding.
const comparableDigest = (digest) =>
  digest.replace(/-/g, "+").replace(/_/g, "/").replace(/=+$/, "");

// Why `bytes` fail the integrity `metadata`, or null when they pass: the
// specification's "do bytes match metadataList", checked against the hashes of
// the strongest algorithm given. Metadata naming no algorithm this library
// knows -- an empty string, whitespace, only "md5-..." -- checks nothing, as
// in `fetch`.
//
// A failure is a TypeError because that is what `fetch` raises for one: a
// network error. `where` names the source in the message.
const integrityRefusal = (bytes, metadata, where) => {
  let hashes = parseIntegrity(metadata);
  if (!hashes.length) return null;
  let algorithm =
      SRI_ALGORITHMS[
        Math.max(...hashes.map((h) => SRI_ALGORITHMS.indexOf(h.algorithm)))
      ],
    digest = createHash(algorithm).update(bytes).digest("base64"),
    actual = comparableDigest(digest);
  if (
    hashes.some(
      (h) => h.algorithm == algorithm && comparableDigest(h.value) === actual,
    )
  )
    return null;
  return TypeError(
    `Integrity check failed for ${where}: its ${algorithm} digest is ` +
      `"${digest}", which is not among the ${algorithm} hashes given`,
  );
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

module.exports = {
  fetchURL,
  decodeDataURL,
  expandURL,
  integrityMetadata,
  integrityRefusal,
};
