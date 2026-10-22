//! `Image::from_url` and `ImageData::from_url` against servers on this
//! machine: no test reaches the network.
//!
//! Each server is a `TcpListener` on `127.0.0.1` answering every connection on
//! its own thread, so a test can see every request it was sent -- including
//! one it must never have been sent. `localhost` reaches the same listener
//! under a different host, which is what a cross-host redirect needs.
#![cfg(feature = "fetch")]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use meo_skia_canvas::{
    Cancel, Image, ImageData, LoadError, LoadOptions, PixelExportOptions,
    Redirect,
};

const PNG: &[u8] = include_bytes!("assets/images/pentagon.png");

/// The SHA-256 of `PNG`, from `openssl dgst -sha256 -binary
/// tests/assets/images/pentagon.png | openssl base64 -A`.
const PNG_SHA256: &str = "Ql6jn9vVPKFySvGSeUJOsakZNmUmbEhintmrRgipg2Y=";

/// How long a stalled server holds its connection: far longer than any
/// cancellation or timeout below is allowed to take.
const STALL: Duration = Duration::from_secs(20);

/// The longest a cancelled or timed-out load may take to return once it
/// should: generous for a loaded CI runner, and still a twentieth of `STALL`,
/// so a load that waited the stall out cannot pass.
const PROMPT: Duration = Duration::from_secs(1);

/// A request as the server saw it.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    /// Header names in lower case.
    headers: Vec<(String, String)>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

enum Reply {
    Full {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// Read the request and never answer.
    StallBeforeResponse,
    /// Send the head and part of a longer body, then go quiet.
    StallMidBody,
}

fn ok(body: &[u8]) -> Reply {
    Reply::Full {
        status: 200,
        headers: vec![],
        body: body.to_vec(),
    }
}

fn redirect(status: u16, location: impl Into<String>) -> Reply {
    Reply::Full {
        status,
        headers: vec![("Location", location.into())],
        body: vec![],
    }
}

struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(
        handler: impl Fn(&Seen, SocketAddr) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let log = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (handler, log) = (handler.clone(), log.clone());
                thread::spawn(move || answer(stream, addr, &*handler, &log));
            }
        });
        Self { addr, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("log").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.path).collect()
    }
}

fn answer(
    mut stream: TcpStream,
    addr: SocketAddr,
    handler: &(dyn Fn(&Seen, SocketAddr) -> Reply + Send + Sync),
    log: &Mutex<Vec<Seen>>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let request = Seen { path, headers };
    log.lock().expect("log").push(request.clone());
    match handler(&request, addr) {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut out = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                out += &format!("{name}: {value}\r\n");
            }
            out += "\r\n";
            let _ = stream.write_all(out.as_bytes());
            let _ = stream.write_all(&body);
        }
        Reply::StallBeforeResponse => thread::sleep(STALL),
        Reply::StallMidBody => {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n");
            let _ = stream.write_all(&PNG[..100]);
            let _ = stream.flush();
            thread::sleep(STALL);
        }
    }
}

fn png_server() -> Server {
    Server::start(|_, _| ok(PNG))
}

#[test]
fn an_image_loads_from_a_url_as_from_its_bytes() {
    let server = png_server();
    let image = Image::from_url(&server.url("/a.png"), &LoadOptions::new())
        .expect("loads");
    let direct = Image::from_encoded(PNG).expect("decodes");
    assert_eq!(
        (image.width(), image.height()),
        (direct.width(), direct.height())
    );
}

#[test]
fn image_data_takes_the_body_as_pixels() {
    let server = Server::start(|_, _| ok(&[7u8; 16]));
    let options = LoadOptions::new();
    let data = ImageData::from_url(
        &server.url("/"),
        2,
        None,
        PixelExportOptions::default(),
        &options,
    )
    .expect("16 bytes are 2 x 2 RGBA");
    assert_eq!(
        (data.width(), data.height(), data.pixels()),
        (2, 2, &[7u8; 16][..])
    );
    // A height that does not fit the body is the constructor's refusal.
    let wrong = ImageData::from_url(
        &server.url("/"),
        2,
        Some(3),
        PixelExportOptions::default(),
        &options,
    );
    assert!(matches!(wrong, Err(LoadError::Decode(_))), "{wrong:?}");
}

#[test]
fn a_data_url_loads_with_no_network() {
    let url = format!("data:image/png;base64,{}", base64_of(PNG));
    let image = Image::from_url(&url, &LoadOptions::new()).expect("loads");
    assert_eq!(
        image.width(),
        Image::from_encoded(PNG).expect("decodes").width()
    );
    let bad = Image::from_url("data:;base64,a", &LoadOptions::new());
    assert!(matches!(bad, Err(LoadError::Url { .. })), "{bad:?}");
}

#[test]
fn a_scheme_it_does_not_load_is_refused_before_any_request() {
    for url in [
        "ftp://127.0.0.1/a.png",
        "file:///etc/hosts",
        "not a url",
        "",
    ] {
        let result = Image::from_url(url, &LoadOptions::new());
        assert!(
            matches!(result, Err(LoadError::Url { .. })),
            "{url:?}: {result:?}"
        );
    }
}

#[test]
fn an_error_status_is_refused_with_it() {
    let server = Server::start(|_, _| Reply::Full {
        status: 404,
        headers: vec![],
        body: b"missing".to_vec(),
    });
    let url = server.url("/gone.png");
    let result = Image::from_url(&url, &LoadOptions::new());
    assert_eq!(result.err(), Some(LoadError::Status { url, status: 404 }));
}

#[test]
fn bytes_that_are_not_an_image_are_a_decode_error() {
    let server = Server::start(|_, _| ok(b"not an image"));
    let result = Image::from_url(&server.url("/"), &LoadOptions::new());
    assert!(matches!(result, Err(LoadError::Decode(_))), "{result:?}");
}

#[test]
fn the_default_user_agent_is_sent_unless_one_is_given() {
    let server = png_server();
    Image::from_url(&server.url("/"), &LoadOptions::new()).expect("loads");
    Image::from_url(
        &server.url("/"),
        &LoadOptions::new().header("user-agent", "mine/1"),
    )
    .expect("loads");
    let agents: Vec<_> = server
        .seen()
        .iter()
        .map(|s| s.header("user-agent").map(str::to_string))
        .collect();
    assert_eq!(
        agents,
        [Some("Skia Canvas".to_string()), Some("mine/1".to_string())]
    );
}

#[test]
fn a_header_that_cannot_be_sent_is_refused_before_any_request() {
    let server = png_server();
    for (name, value) in [("bad name", "x"), ("x-ok", "line\nbreak")] {
        let result = Image::from_url(
            &server.url("/"),
            &LoadOptions::new().header(name, value),
        );
        assert!(
            matches!(result, Err(LoadError::Header { .. })),
            "{name:?}: {result:?}"
        );
    }
    assert!(server.paths().is_empty());
}

// -- Redirects --------------------------------------------------------------

/// `/start` redirects to `location`; anything else is the PNG.
fn redirecting(status: u16, location: &'static str) -> Server {
    Server::start(move |seen, _| match seen.path.as_str() {
        "/start" => redirect(status, location),
        _ => ok(PNG),
    })
}

#[test]
fn every_redirect_status_is_followed_and_a_relative_location_resolves() {
    for status in [301, 302, 303, 307, 308] {
        let server = redirecting(status, "../img/b.png?x=1");
        let image = Image::from_url(&server.url("/start"), &LoadOptions::new());
        assert!(image.is_ok(), "{status}: {image:?}");
        assert_eq!(server.paths(), ["/start", "/img/b.png?x=1"], "{status}");
    }
}

#[test]
fn a_3xx_that_is_not_a_redirect_status_is_an_error_status() {
    // Fetch redirects on 301, 302, 303, 307 and 308 only; a 300 with a
    // Location is a response like any other non-2xx.
    let server = redirecting(300, "/b.png");
    let url = server.url("/start");
    let result = Image::from_url(&url, &LoadOptions::new());
    assert_eq!(result.err(), Some(LoadError::Status { url, status: 300 }));
    assert_eq!(server.paths(), ["/start"]);
}

#[test]
fn error_and_manual_refuse_at_the_redirect_and_never_ask_the_target() {
    for mode in [Redirect::Error, Redirect::Manual] {
        let server = redirecting(302, "/target.png");
        let url = server.url("/start");
        let result = Image::from_url(&url, &LoadOptions::new().redirect(mode));
        assert_eq!(
            result.err(),
            Some(LoadError::Redirect {
                url: url.clone(),
                status: 302,
                location: server.url("/target.png"),
                mode,
            }),
        );
        assert_eq!(server.paths(), ["/start"], "{mode:?}");
    }
}

#[test]
fn twenty_redirects_are_followed_and_a_twenty_first_is_refused() {
    // `/n` redirects to `/n-1`, and `/0` is the image.
    let server = Server::start(|seen, _| {
        let n: u32 = seen.path[1..].parse().unwrap_or(0);
        if n == 0 {
            ok(PNG)
        } else {
            redirect(302, format!("/{}", n - 1))
        }
    });
    assert!(Image::from_url(&server.url("/20"), &LoadOptions::new()).is_ok());
    let url = server.url("/21");
    let result = Image::from_url(&url, &LoadOptions::new());
    assert_eq!(result.err(), Some(LoadError::TooManyRedirects { url }));
}

#[test]
fn credentials_stay_on_the_host_they_were_given_for() {
    // `/start` sends the request to the other host name; `/next` stays.
    let server = Server::start(|seen, addr| match seen.path.as_str() {
        "/away" => {
            redirect(302, format!("http://localhost:{}/landed", addr.port()))
        }
        "/stay" => redirect(302, "/landed"),
        _ => ok(PNG),
    });
    let options = LoadOptions::new()
        .header("Authorization", "Bearer secret")
        .header("Cookie", "id=1")
        .header("Proxy-Authorization", "Basic x")
        .header("X-Api-Key", "k");
    Image::from_url(&server.url("/away"), &options).expect("loads");
    Image::from_url(&server.url("/stay"), &options).expect("loads");
    let seen = server.seen();
    let names = |s: &Seen| {
        [
            "authorization",
            "cookie",
            "proxy-authorization",
            "x-api-key",
        ]
        .into_iter()
        .filter(|n| s.header(n).is_some())
        .collect::<Vec<_>>()
    };
    assert_eq!(seen.len(), 4);
    let all = [
        "authorization",
        "cookie",
        "proxy-authorization",
        "x-api-key",
    ];
    assert_eq!(names(&seen[0]), all, "first request, before the redirect");
    assert_eq!(
        names(&seen[1]),
        ["x-api-key"],
        "after a redirect to another host"
    );
    assert_eq!(names(&seen[3]), all, "after a redirect on the same host");
}

// -- Integrity --------------------------------------------------------------

#[test]
fn integrity_is_checked_against_the_body_after_redirects() {
    let server = redirecting(302, "/b.png");
    let url = server.url("/start");
    let good = LoadOptions::new().integrity(&format!("sha256-{PNG_SHA256}"));
    assert!(Image::from_url(&url, &good).is_ok());
    let bad = LoadOptions::new().integrity("sha256-AAAA");
    match Image::from_url(&url, &bad) {
        Err(LoadError::Integrity { url: named, error }) => {
            assert_eq!(named, url);
            assert_eq!(error.actual, PNG_SHA256);
        }
        other => panic!("expected an integrity refusal, got {other:?}"),
    }
    // A data: URL is checked too.
    let data = format!("data:image/png;base64,{}", base64_of(PNG));
    assert!(matches!(
        Image::from_url(&data, &bad),
        Err(LoadError::Integrity { .. })
    ));
}

// -- Timeouts and cancellation ----------------------------------------------

fn stalling(reply: fn() -> Reply) -> Server {
    Server::start(move |_, _| reply())
}

/// Runs `load` on another thread, cancels `cancel` after `after`, and returns
/// the result with how long the load took from the cancellation on.
fn cancel_after(
    cancel: &Cancel,
    after: Duration,
    load: impl FnOnce() -> Result<Image, LoadError> + Send + 'static,
) -> (Result<Image, LoadError>, Duration) {
    let worker = thread::spawn(load);
    thread::sleep(after);
    let cancelled = Instant::now();
    cancel.cancel();
    let result = worker.join().expect("the load thread");
    (result, cancelled.elapsed())
}

#[test]
fn a_timeout_ends_a_load_whose_server_never_answers() {
    for reply in [(|| Reply::StallBeforeResponse) as fn() -> Reply, || {
        Reply::StallMidBody
    }] {
        let server = stalling(reply);
        let url = server.url("/");
        let started = Instant::now();
        let result = Image::from_url(
            &url,
            &LoadOptions::new().timeout(Duration::from_millis(200)),
        );
        let took = started.elapsed();
        assert!(
            matches!(result, Err(LoadError::Timeout { .. })),
            "{result:?}"
        );
        assert!(
            took >= Duration::from_millis(200) && took < PROMPT,
            "{took:?}"
        );
    }
}

#[test]
fn cancelling_interrupts_a_server_that_never_answers() {
    let server = stalling(|| Reply::StallBeforeResponse);
    let (cancel, url) = (Cancel::new(), server.url("/"));
    let options = LoadOptions::new().cancel(&cancel);
    let (result, took) = cancel_after(&cancel, Duration::from_millis(200), {
        let url = url.clone();
        move || Image::from_url(&url, &options)
    });
    assert_eq!(result.err(), Some(LoadError::Cancelled { url }));
    assert!(took < PROMPT, "{took:?}");
}

#[test]
fn cancelling_interrupts_a_body_that_stops_arriving() {
    let server = stalling(|| Reply::StallMidBody);
    let (cancel, url) = (Cancel::new(), server.url("/"));
    let options = LoadOptions::new().cancel(&cancel);
    let (result, took) = cancel_after(&cancel, Duration::from_millis(200), {
        let url = url.clone();
        move || Image::from_url(&url, &options)
    });
    assert_eq!(result.err(), Some(LoadError::Cancelled { url }));
    assert!(took < PROMPT, "{took:?}");
}

#[test]
fn a_cancelled_token_refuses_before_any_request() {
    let server = png_server();
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(cancel.is_cancelled());
    let url = server.url("/");
    let result = Image::from_url(&url, &LoadOptions::new().cancel(&cancel));
    assert_eq!(result.err(), Some(LoadError::Cancelled { url }));
    assert!(server.paths().is_empty());
}

#[test]
fn one_token_cancels_every_load_it_was_given_to() {
    let server = stalling(|| Reply::StallBeforeResponse);
    let cancel = Cancel::new();
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let (url, options) =
                (server.url("/"), LoadOptions::new().cancel(&cancel));
            thread::spawn(move || Image::from_url(&url, &options))
        })
        .collect();
    thread::sleep(Duration::from_millis(200));
    let cancelled = Instant::now();
    cancel.cancel();
    for worker in workers {
        let result = worker.join().expect("the load thread");
        assert!(
            matches!(result, Err(LoadError::Cancelled { .. })),
            "{result:?}"
        );
    }
    assert!(cancelled.elapsed() < PROMPT);
}

#[test]
fn a_finished_load_is_unaffected_by_a_later_cancel() {
    let server = png_server();
    let cancel = Cancel::new();
    let image =
        Image::from_url(&server.url("/"), &LoadOptions::new().cancel(&cancel));
    cancel.cancel();
    assert!(image.is_ok());
}

#[test]
fn the_cancel_token_crosses_threads() {
    fn shared<T: Clone + Send + Sync + 'static>() {}
    shared::<Cancel>();
    shared::<LoadOptions>();
    fn error<T: std::error::Error + Send + Sync + 'static>() {}
    error::<LoadError>();
}

fn base64_of(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    bytes
        .chunks(3)
        .flat_map(|chunk| {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            (0..4).map(move |i| {
                if i <= chunk.len() {
                    ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
                } else {
                    '='
                }
            })
        })
        .collect()
}
