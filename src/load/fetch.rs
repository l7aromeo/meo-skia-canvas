use std::{
    fmt,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use ureq::{
    Agent,
    http::{HeaderName, HeaderValue},
};

use crate::{
    Error, Image, ImageData, Integrity, IntegrityError, PixelExportOptions,
    decode_data_url,
};

/// What a load does when the server answers with a redirect: the Fetch
/// Standard's `RequestRedirect`, which npm's loaders take as `redirect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Redirect {
    /// Follow it, up to 20 in one load, as `fetch` does. `Authorization`,
    /// `Cookie` and `Proxy-Authorization` are not sent on past a redirect to
    /// another host, or from `https` to `http`.
    #[default]
    Follow,
    /// Refuse the load with [`LoadError::Redirect`] at the first redirect.
    Error,
    /// Hand the redirect back as [`LoadError::Redirect`], naming where it
    /// leads, for the caller to decide about. A redirect has no image in it,
    /// so there is nothing else a load could return.
    Manual,
}

/// The most redirects one load follows before refusing: the Fetch Standard's
/// limit, "if request's redirect count is 20, then return a network error".
const MAX_REDIRECTS: u32 = 20;

/// The statuses Fetch treats as a redirect. A 300, 304, 305 or 306 with a
/// `Location` is a response like any other.
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// The `User-Agent` a load sends unless the caller gives one: the npm
/// package's, so a server sees one client whichever surface loaded.
const USER_AGENT: &str = "Skia Canvas";

/// The request headers that carry a credential, which a redirect to another
/// host does not receive: follow-redirects' list, which npm's loaders use.
const CREDENTIAL_HEADERS: [&str; 3] =
    ["authorization", "cookie", "proxy-authorization"];

/// How much of a body one read asks for.
const READ_CHUNK: usize = 64 * 1024;

/// Characters percent-encoded in a URL before it is requested, as a browser
/// does: controls, space, and the characters RFC 3986 excludes and an
/// `http::Uri` refuses. Bytes outside ASCII are always encoded.
const URL_UNSAFE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// Cancels loads from another thread.
///
/// Give a clone to each load's [`LoadOptions::cancel`] and call
/// [`cancel`](Self::cancel) from anywhere: every load holding the token
/// returns [`LoadError::Cancelled`], and a load given an already cancelled
/// token returns it before making a request. A token stays cancelled.
///
/// **Where it takes effect.** Each load's connections are closed under it.
/// A load waiting on the server -- in a TLS handshake, for its response, or
/// for more of its body -- returns at once, and one between those fails its
/// next read or its next request, so a redirect is not followed. Two waits
/// cannot be interrupted, because the operating system call making them
/// cannot: resolving the host name, and opening the TCP connection. A load
/// cancelled during either returns `Cancelled` when that call returns:
/// within [`LoadOptions::timeout`] when one is set, and otherwise when the
/// operating system gives up.
///
/// ```no_run
/// use meo_skia_canvas::{Cancel, Image, LoadError, LoadOptions};
/// use std::{thread, time::Duration};
///
/// let cancel = Cancel::new();
/// let options = LoadOptions::new().cancel(&cancel);
/// let load = thread::spawn(move || {
///     Image::from_url("https://example.com/big.png", &options)
/// });
/// thread::sleep(Duration::from_millis(100));
/// cancel.cancel();
/// assert!(matches!(
///     load.join().unwrap(),
///     Err(LoadError::Cancelled { .. })
/// ));
/// ```
#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    /// The agents of loads running with this token, each under an id its
    /// load removes it by when it finishes.
    agents: Mutex<Vec<(u64, Agent)>>,
    next_id: AtomicU64,
}

impl Cancel {
    /// A token that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every load holding this token, now and later.
    pub fn cancel(&self) {
        // The flag before the list: a load registering after the list is
        // taken sees the flag once it holds the lock, because the lock orders
        // the two.
        self.0.cancelled.store(true, Ordering::SeqCst);
        let agents = std::mem::take(&mut *self.0.agents.lock());
        for (_, agent) in agents {
            agent.shutdown();
        }
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Puts `agent` where [`cancel`](Self::cancel) can shut it down, until
    /// the returned guard drops. `None` when the token is already cancelled.
    fn register(&self, agent: &Agent) -> Option<Registration<'_>> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        self.0.agents.lock().push((id, agent.clone()));
        let registration = Registration { cancel: self, id };
        (!self.is_cancelled()).then_some(registration)
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cancel")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Two tokens are equal when they are clones of one another.
impl PartialEq for Cancel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A load's agent, listed in its [`Cancel`] while the load runs.
struct Registration<'a> {
    cancel: &'a Cancel,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.cancel.0.agents.lock().retain(|(id, _)| *id != self.id);
    }
}

/// How [`Image::from_url`] and [`ImageData::from_url`] load: the parts of
/// `RequestInit` npm's `loadImage` reads, and a timeout and cancellation in
/// place of an `AbortSignal`.
///
/// Built with its methods, from [`LoadOptions::new`]:
///
/// ```
/// use meo_skia_canvas::{LoadOptions, Redirect};
/// use std::time::Duration;
///
/// let options = LoadOptions::new()
///     .header("Accept", "image/png")
///     .redirect(Redirect::Error)
///     .integrity("sha256-Ql6jn9vVPKFySvGSeUJOsakZNmUmbEhintmrRgipg2Y=")
///     .timeout(Duration::from_secs(10));
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadOptions {
    headers: Vec<(String, String)>,
    redirect: Redirect,
    integrity: Integrity,
    timeout: Option<Duration>,
    cancel: Option<Cancel>,
}

impl LoadOptions {
    /// No headers beyond the default `User-Agent`, redirects followed,
    /// nothing checked, no timeout and no cancellation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a request header. A name given twice is sent twice. A
    /// `User-Agent` replaces the default, `Skia Canvas`.
    ///
    /// A name or value HTTP cannot carry is refused when the load starts,
    /// with [`LoadError::Header`], before any request.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// What to do at a redirect; [`Redirect::Follow`] unless set.
    pub fn redirect(mut self, mode: Redirect) -> Self {
        self.redirect = mode;
        self
    }

    /// Subresource Integrity metadata the bytes must match, parsed with
    /// [`Integrity::parse`]. It is checked against the whole body, after any
    /// redirects and before decoding, and for a `data:` URL as well.
    pub fn integrity(mut self, metadata: &str) -> Self {
        self.integrity = Integrity::parse(metadata);
        self
    }

    /// The longest the whole load may take, from resolving the host name
    /// through the last byte of the body and across every redirect. A load
    /// still running then returns [`LoadError::Timeout`]. No limit unless
    /// set.
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    /// Lets `cancel` stop the load from another thread; see [`Cancel`] for
    /// where that takes effect.
    pub fn cancel(mut self, cancel: &Cancel) -> Self {
        self.cancel = Some(cancel.clone());
        self
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(Cancel::is_cancelled)
    }
}

/// Why a load from a URL failed.
///
/// Every variant naming a `url` names the one the load was given, not where
/// a redirect led.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LoadError {
    /// The URL could not be loaded as written: it does not parse, its scheme
    /// is not `http`, `https` or `data`, it is a `data:` URL that does not
    /// decode, or a redirect led to one of these.
    Url {
        /// The URL given.
        url: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A header from [`LoadOptions::header`] that HTTP cannot carry.
    Header {
        /// The header's name.
        name: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The request failed below HTTP: the host was not found, the connection
    /// was refused or dropped, TLS failed.
    Network {
        /// The URL given.
        url: String,
        /// The failure, as the HTTP client reported it.
        reason: String,
    },
    /// [`LoadOptions::timeout`] passed before the load finished.
    Timeout {
        /// The URL given.
        url: String,
    },
    /// The load's [`Cancel`] was cancelled.
    Cancelled {
        /// The URL given.
        url: String,
    },
    /// The server answered with a status outside 200 to 299 that is not a
    /// redirect being followed.
    Status {
        /// The URL given.
        url: String,
        /// The HTTP status.
        status: u16,
    },
    /// A redirect that [`Redirect::Error`] or [`Redirect::Manual`] says not
    /// to follow. Where it leads is never requested.
    Redirect {
        /// The URL given.
        url: String,
        /// The redirect's HTTP status.
        status: u16,
        /// Where it leads, resolved against the URL that answered with it.
        location: String,
        /// The mode that stopped the load.
        mode: Redirect,
    },
    /// The load met a twenty-first redirect.
    TooManyRedirects {
        /// The URL given.
        url: String,
    },
    /// The bytes did not match [`LoadOptions::integrity`].
    Integrity {
        /// The URL given.
        url: String,
        /// The algorithm checked and the digest the bytes have.
        error: IntegrityError,
    },
    /// The bytes arrived and are not what was asked for: not an image, or
    /// not the length of the pixels [`ImageData::from_url`] was told to
    /// expect.
    Decode(Error),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url { url, reason } => {
                write!(f, "Cannot load \"{url}\": {reason}")
            }
            Self::Header { name, reason } => {
                write!(f, "Cannot send the request header {name:?}: {reason}")
            }
            Self::Network { url, reason } => {
                write!(f, "Failed to load \"{url}\": {reason}")
            }
            Self::Timeout { url } => write!(f, "Loading \"{url}\" timed out"),
            Self::Cancelled { url } => {
                write!(f, "Loading \"{url}\" was cancelled")
            }
            Self::Status { url, status } => {
                write!(
                    f,
                    "Failed to load image from \"{url}\" (HTTP error {status})"
                )
            }
            Self::Redirect {
                url,
                status,
                location,
                mode: Redirect::Error,
            } => write!(
                f,
                "Refused to follow a redirect from \"{url}\" to \"{location}\" (HTTP {status}), \
                 as the redirect mode is `Redirect::Error`"
            ),
            Self::Redirect {
                url,
                status,
                location,
                ..
            } => write!(
                f,
                "\"{url}\" redirects to \"{location}\" (HTTP {status}), which \
                 `Redirect::Manual` leaves to the caller to follow"
            ),
            Self::TooManyRedirects { url } => write!(
                f,
                "Loading \"{url}\" met more than {MAX_REDIRECTS} redirects"
            ),
            Self::Integrity { url, error } => {
                write!(f, "Integrity check failed for \"{url}\": {error}")
            }
            Self::Decode(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Integrity { error, .. } => Some(error),
            Self::Decode(error) => Some(error),
            _ => None,
        }
    }
}

impl From<Error> for LoadError {
    fn from(error: Error) -> Self {
        Self::Decode(error)
    }
}

impl Image {
    /// Loads and decodes the image at `url`: `http`, `https` or `data`, as
    /// npm's `loadImage(url, options)` does. Blocks until the image has
    /// arrived; call it from a thread that may wait, or through
    /// `spawn_blocking` in an async program. Needs the `fetch` feature.
    ///
    /// TLS is rustls with the Mozilla root certificates webpki-roots
    /// carries, not the operating system's store. A proxy is taken from
    /// `ALL_PROXY`, `HTTPS_PROXY` or `HTTP_PROXY` (either case) unless
    /// `NO_PROXY` exempts the host.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] saying which step failed.
    pub fn from_url(
        url: &str,
        options: &LoadOptions,
    ) -> Result<Image, LoadError> {
        let bytes = load_bytes(url, options)?;
        Ok(Image::from_encoded(&bytes)?)
    }
}

impl ImageData {
    /// Loads the bytes at `url` as pixels in `settings`' layout, as npm's
    /// `loadImageData(url, width, height, settings)` does: the body is the
    /// pixels, not an encoded image. With `height` as `None`, it is the
    /// number of whole rows the body holds. Blocks as [`Image::from_url`]
    /// does, and needs the `fetch` feature.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] saying which step failed, and
    /// [`LoadError::Decode`] holding [`ImageData::from_pixels`]'s refusal
    /// when the body is not `width` by `height` pixels.
    pub fn from_url(
        url: &str,
        width: u32,
        height: Option<u32>,
        settings: PixelExportOptions,
        options: &LoadOptions,
    ) -> Result<ImageData, LoadError> {
        let bytes = load_bytes(url, options)?;
        let height = height.unwrap_or_else(|| {
            let stride = width as usize * settings.depth.bytes_per_pixel();
            let rows = bytes.len().checked_div(stride).unwrap_or(0);
            u32::try_from(rows).unwrap_or(u32::MAX)
        });
        Ok(ImageData::from_pixels(width, height, settings, bytes)?)
    }
}

/// The bytes at `url`, checked against the options' integrity.
fn load_bytes(url: &str, options: &LoadOptions) -> Result<Vec<u8>, LoadError> {
    let refused = |reason: String| LoadError::Url {
        url: url.to_string(),
        reason,
    };
    if options.is_cancelled() {
        return Err(LoadError::Cancelled {
            url: url.to_string(),
        });
    }
    let bytes = match scheme(url).map(str::to_ascii_lowercase).as_deref() {
        Some("data") => {
            decode_data_url(url).map_err(|e| refused(e.to_string()))?
        }
        Some("http" | "https") => fetch(url, options)?,
        Some(other) => {
            return Err(refused(format!(
                "a {other}: URL is not loaded; expected http, https or data"
            )));
        }
        None => return Err(refused("it is not a URL".to_string())),
    };
    options
        .integrity
        .verify(&bytes)
        .map_err(|error| LoadError::Integrity {
            url: url.to_string(),
            error,
        })?;
    Ok(bytes)
}

/// The scheme of `url`, RFC 3986's `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`
/// before the first `:`, after the C0 controls and spaces a URL parser
/// ignores at the start.
fn scheme(url: &str) -> Option<&str> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Requests `url`, following redirects as the options say, and reads the
/// whole body.
fn fetch(url: &str, options: &LoadOptions) -> Result<Vec<u8>, LoadError> {
    let deadline = options.timeout.map(|limit| Instant::now() + limit);
    for (name, value) in &options.headers {
        let refused = |reason: String| LoadError::Header {
            name: name.clone(),
            reason,
        };
        HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| refused(e.to_string()))?;
        HeaderValue::from_str(value).map_err(|e| refused(e.to_string()))?;
    }
    // One agent per load, so that cancelling it closes this load's
    // connections and no one else's. Redirects are followed here rather than
    // by the agent, so that the limit, the statuses and the credentials
    // dropped at a host change are this crate's and not the client's.
    let agent: Agent = Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into();
    let _registration = match &options.cancel {
        Some(cancel) => Some(cancel.register(&agent).ok_or_else(|| {
            LoadError::Cancelled {
                url: url.to_string(),
            }
        })?),
        None => None,
    };
    let fail = |error: ureq::Error| failure(error, url, options);
    let mut headers = options.headers.clone();
    let mut current = url.trim_matches(|c: char| c <= ' ').to_string();
    let mut redirects = 0;
    loop {
        let remaining = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if remaining == Some(Duration::ZERO) {
            return Err(LoadError::Timeout {
                url: url.to_string(),
            });
        }
        let target = request_target(&current);
        let request = headers
            .iter()
            .fold(agent.get(&target), |request, (name, value)| {
                request.header(name, value)
            })
            .config()
            .timeout_global(remaining)
            .build();
        let response = request.call().map_err(fail)?;
        let status = response.status().as_u16();
        let location = response.headers().get("location").map(|value| {
            String::from_utf8_lossy(value.as_bytes()).into_owned()
        });
        if let Some(location) =
            location.filter(|_| REDIRECT_STATUSES.contains(&status))
        {
            let next = resolve(&current, &location);
            if options.redirect != Redirect::Follow {
                return Err(LoadError::Redirect {
                    url: url.to_string(),
                    status,
                    location: next,
                    mode: options.redirect,
                });
            }
            if redirects == MAX_REDIRECTS {
                return Err(LoadError::TooManyRedirects {
                    url: url.to_string(),
                });
            }
            if !matches!(
                scheme(&next).map(str::to_ascii_lowercase).as_deref(),
                Some("http" | "https")
            ) {
                return Err(LoadError::Url {
                    url: url.to_string(),
                    reason: format!(
                        "it redirects to \"{next}\", which is not an http or https URL"
                    ),
                });
            }
            if !keeps_credentials(&current, &next) {
                headers.retain(|(name, _)| {
                    !CREDENTIAL_HEADERS
                        .iter()
                        .any(|credential| name.eq_ignore_ascii_case(credential))
                });
            }
            current = next;
            redirects += 1;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(LoadError::Status {
                url: url.to_string(),
                status,
            });
        }
        let mut reader = response.into_body().into_reader();
        let mut body = Vec::new();
        let mut chunk = vec![0; READ_CHUNK];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => return Ok(body),
                Ok(read) => body.extend_from_slice(&chunk[..read]),
                Err(error) => return Err(fail(ureq::Error::from(error))),
            }
        }
    }
}

/// What a client error means for this load. Cancellation is checked first,
/// because a cancelled load fails with whatever the closed connection
/// produced -- an I/O error, usually.
fn failure(error: ureq::Error, url: &str, options: &LoadOptions) -> LoadError {
    let url = url.to_string();
    if options.is_cancelled() {
        return LoadError::Cancelled { url };
    }
    match error {
        ureq::Error::Timeout(_) => LoadError::Timeout { url },
        ureq::Error::Io(error)
            if error.kind() == std::io::ErrorKind::TimedOut =>
        {
            LoadError::Timeout { url }
        }
        ureq::Error::BadUri(reason) => LoadError::Url { url, reason },
        ureq::Error::Http(error) => LoadError::Url {
            url,
            reason: error.to_string(),
        },
        error => LoadError::Network {
            url,
            reason: error.to_string(),
        },
    }
}

/// `url` as it is sent: without its fragment, which is never part of a
/// request, and with the characters a request line cannot carry
/// percent-encoded, as a browser encodes them.
fn request_target(url: &str) -> String {
    let url = url.split_once('#').map_or(url, |(before, _)| before);
    utf8_percent_encode(url, URL_UNSAFE).to_string()
}

/// A URL split into RFC 3986's five components, by the regular expression in
/// its Appendix B.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Components<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl<'a> Components<'a> {
    fn split(reference: &'a str) -> Self {
        let (rest, fragment) = match reference.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment)),
            None => (reference, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, Some(query)),
            None => (rest, None),
        };
        let (scheme, rest) = match rest.split_once(':') {
            Some((scheme, rest))
                if !scheme.is_empty() && !scheme.contains('/') =>
            {
                (Some(scheme), rest)
            }
            _ => (None, rest),
        };
        let (authority, path) = match rest.strip_prefix("//") {
            Some(rest) => {
                let end = rest.find('/').unwrap_or(rest.len());
                (Some(&rest[..end]), &rest[end..])
            }
            None => (None, rest),
        };
        Self {
            scheme,
            authority,
            path,
            query,
            fragment,
        }
    }

    /// The host and port, as WHATWG's `URL.host` gives them: lower case,
    /// with any user information dropped and a scheme's default port left
    /// out.
    fn host(&self) -> String {
        let authority = self.authority.unwrap_or_default();
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let authority = authority.to_ascii_lowercase();
        let default_port =
            match self.scheme.map(str::to_ascii_lowercase).as_deref() {
                Some("http") => ":80",
                Some("https") => ":443",
                _ => return authority,
            };
        authority
            .strip_suffix(default_port)
            .map_or_else(|| authority.clone(), str::to_string)
    }
}

/// `reference` resolved against `base`: RFC 3986, section 5.2.2, with the
/// result recomposed by section 5.3.
fn resolve(base: &str, reference: &str) -> String {
    let base = Components::split(base);
    let r = Components::split(reference);
    let (scheme, authority, path, query) = if r.scheme.is_some() {
        (r.scheme, r.authority, remove_dot_segments(r.path), r.query)
    } else if r.authority.is_some() {
        (
            base.scheme,
            r.authority,
            remove_dot_segments(r.path),
            r.query,
        )
    } else if r.path.is_empty() {
        (
            base.scheme,
            base.authority,
            base.path.to_string(),
            r.query.or(base.query),
        )
    } else if r.path.starts_with('/') {
        (
            base.scheme,
            base.authority,
            remove_dot_segments(r.path),
            r.query,
        )
    } else {
        let merged = match base.path.rfind('/') {
            None if base.authority.is_some() => format!("/{}", r.path),
            None => r.path.to_string(),
            Some(slash) => format!("{}{}", &base.path[..=slash], r.path),
        };
        (
            base.scheme,
            base.authority,
            remove_dot_segments(&merged),
            r.query,
        )
    };
    let mut out = String::new();
    if let Some(scheme) = scheme {
        out += scheme;
        out.push(':');
    }
    if let Some(authority) = authority {
        out += "//";
        out += authority;
    }
    out += &path;
    if let Some(query) = query {
        out.push('?');
        out += query;
    }
    if let Some(fragment) = r.fragment {
        out.push('#');
        out += fragment;
    }
    out
}

/// RFC 3986, section 5.2.4.
fn remove_dot_segments(path: &str) -> String {
    let mut input = path;
    let mut output = String::new();
    while !input.is_empty() {
        if let Some(rest) = input
            .strip_prefix("../")
            .or_else(|| input.strip_prefix("./"))
        {
            input = rest;
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") || input == "/.." {
            input = if input == "/.." { "/" } else { &input[3..] };
            let cut = output.rfind('/').unwrap_or(0);
            output.truncate(cut);
        } else if input == "." || input == ".." {
            input = "";
        } else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..]
                .find('/')
                .map_or(input.len(), |at| at + start);
            output += &input[..end];
            input = &input[end..];
        }
    }
    output
}

/// Whether a redirect from `from` to `to` keeps the credential headers:
/// follow-redirects' rule, which npm's loaders apply. They are dropped when
/// the scheme changes to anything but `https`, and when the host changes to
/// one that is not a subdomain of it. The port is part of the host.
fn keeps_credentials(from: &str, to: &str) -> bool {
    let (from, to) = (Components::split(from), Components::split(to));
    let lower = |scheme: Option<&str>| scheme.map(str::to_ascii_lowercase);
    let to_scheme = lower(to.scheme);
    if to_scheme != lower(from.scheme) && to_scheme.as_deref() != Some("https")
    {
        return false;
    }
    let (from_host, to_host) = (from.host(), to.host());
    to_host == from_host || is_subdomain(&to_host, &from_host)
}

/// follow-redirects' `isSubdomain`: `sub` ends in `.` and then `domain`.
fn is_subdomain(sub: &str, domain: &str) -> bool {
    sub.strip_suffix(domain)
        .is_some_and(|head| head.len() > 1 && head.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3986, sections 5.4.1 and 5.4.2: every normal and abnormal example,
    /// against the base URI the section gives.
    #[test]
    fn references_resolve_as_rfc_3986_says() {
        const BASE: &str = "http://a/b/c/d;p?q";
        let examples = [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g#s/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
            ("http:g", "http:g"),
        ];
        let wrong: Vec<_> = examples
            .iter()
            .filter(|(reference, want)| resolve(BASE, reference) != *want)
            .map(|(reference, want)| {
                format!(
                    "{reference:?}: {:?}, want {want:?}",
                    resolve(BASE, reference)
                )
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A load that registers after `cancel` has taken the list of agents
    /// is never shut down, so it has to learn of the cancellation from the
    /// registration itself. The window is too narrow to reach through
    /// `Image::from_url`.
    #[test]
    fn a_cancelled_token_refuses_to_register_a_load() {
        let agent = Agent::new_with_defaults();
        let cancel = Cancel::new();
        assert!(cancel.register(&agent).is_some());
        assert!(
            cancel.0.agents.lock().is_empty(),
            "a finished load leaves the list"
        );
        cancel.cancel();
        assert!(cancel.register(&agent).is_none());
        assert!(cancel.0.agents.lock().is_empty());
    }

    #[test]
    fn credentials_follow_the_follow_redirects_rule() {
        let cases = [
            ("http://a.com/x", "http://a.com/y", true),
            ("http://a.com/x", "http://A.COM:80/y", true),
            ("http://a.com/x", "https://a.com/y", true),
            ("https://a.com/x", "http://a.com/y", false),
            ("http://a.com/x", "http://b.com/y", false),
            ("http://a.com/x", "http://a.com:8080/y", false),
            ("http://a.com/x", "http://cdn.a.com/y", true),
            ("http://cdn.a.com/x", "http://a.com/y", false),
            ("http://a.com/x", "http://evila.com/y", false),
            ("http://user@a.com/x", "http://a.com/y", true),
        ];
        for (from, to, keeps) in cases {
            assert_eq!(keeps_credentials(from, to), keeps, "{from} -> {to}");
        }
    }

    #[test]
    fn a_scheme_is_found_only_where_rfc_3986_allows_one() {
        assert_eq!(scheme("HTTP://a"), Some("HTTP"));
        assert_eq!(scheme("  data:,x"), Some("data"));
        assert_eq!(scheme("c+d.e-f:x"), Some("c+d.e-f"));
        assert_eq!(scheme("1http://a"), None);
        assert_eq!(scheme("/a:b"), None);
        assert_eq!(scheme("not a url"), None);
        assert_eq!(scheme(""), None);
    }

    #[test]
    fn a_request_target_drops_its_fragment_and_encodes_what_a_request_cannot_carry()
     {
        assert_eq!(request_target("http://a/b c#frag"), "http://a/b%20c");
        assert_eq!(
            request_target("http://a/é?q=\"x\""),
            "http://a/%C3%A9?q=%22x%22"
        );
        assert_eq!(request_target("http://a/%41?b=c&d"), "http://a/%41?b=c&d");
    }
}
