const url = require("url"),
  { http, https } = require("follow-redirects"),
  { HttpsProxyAgent } = require("https-proxy-agent");

const UA = { "User-Agent": "Skia Canvas" };
const PROXY_URL =
  process.env.https_proxy ||
  process.env.HTTPS_PROXY ||
  process.env.http_proxy ||
  process.env.HTTP_PROXY;

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
    opts.headers = { ...UA, ...opts.headers };
    opts.agent =
      opts.agent === undefined && PROXY_URL
        ? new HttpsProxyAgent(PROXY_URL)
        : opts.agent;

    let req = client.request(url, opts, (resp) => {
      if (resp.statusCode < 200 || resp.statusCode >= 300) {
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
