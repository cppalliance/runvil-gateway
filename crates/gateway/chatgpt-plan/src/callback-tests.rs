//! The callback listener against the real broker and the fake issuer.
//!
//! One connection is one HTTP/1.1 request. The listener is still up when a
//! later request is answered; it has closed when the next connect fails.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use url::form_urlencoded;

use super::{CallbackDone, CallbackError, CallbackListener};
use crate::fake_issuer::Fake;
use crate::plan::{ChatGptPlan, Clock};

const PAGE_WAIT: Duration = Duration::from_secs(5);

struct Rig {
    fake: Fake,
    _dir: tempfile::TempDir,
    plan: ChatGptPlan,
    now: Arc<AtomicU64>,
    clock: Clock,
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
    }
    dir
}

impl Rig {
    async fn new() -> Rig {
        let fake = Fake::start().await;
        let dir = private_dir();
        let real_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let now = Arc::new(AtomicU64::new(real_now));
        let clock: Clock = {
            let now = now.clone();
            Arc::new(move || now.load(Ordering::SeqCst))
        };
        let plan = ChatGptPlan::open_with_clock(dir.path(), fake.config(), clock.clone())
            .expect("the broker opens");
        Rig {
            fake,
            _dir: dir,
            plan,
            now,
            clock,
        }
    }

    fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    async fn listen(&self) -> CallbackListener {
        CallbackListener::bind(self.clock.clone())
            .await
            .expect("binds")
    }

    /// Starts a sign-in on `listener` and teaches the fake that attempt.
    async fn begin(&self, listener: &CallbackListener) -> String {
        let url = self
            .plan
            .begin_sign_in(listener.redirect_uri())
            .await
            .expect("begins");
        let query: HashMap<String, String> = url::Url::parse(&url)
            .expect("a url")
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        self.fake
            .set(|behavior| behavior.nonce.clone_from(&query["nonce"]));
        self.fake
            .expect_exchange(&query["code_challenge"], &query["redirect_uri"]);
        query["state"].clone()
    }
}

fn escape(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn addr_of(listener: &CallbackListener) -> SocketAddr {
    let url = url::Url::parse(listener.redirect_uri()).expect("redirect uri");
    SocketAddr::from((Ipv4Addr::LOCALHOST, url.port().expect("an ephemeral port")))
}

/// `GET /auth/callback?query HTTP/1.1` with a short header block.
fn get(query: &str) -> String {
    format!("GET /auth/callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
}

fn success(state: &str) -> String {
    get(&format!(
        "state={}&code=code-1&scope=not+forwarded&client_id=oaiapp_1",
        escape(state)
    ))
}

fn spawn_serve(
    plan: ChatGptPlan,
    listener: CallbackListener,
) -> tokio::task::JoinHandle<Result<CallbackDone, CallbackError>> {
    tokio::spawn(async move { listener.serve(&plan).await })
}

async fn finish(
    task: tokio::task::JoinHandle<Result<CallbackDone, CallbackError>>,
) -> CallbackDone {
    tokio::time::timeout(PAGE_WAIT, task)
        .await
        .expect("the listener is still running")
        .expect("the listener task panicked")
        .expect("the listener failed")
}

/// Writes `request` and reads the reply until the listener closes its half.
/// The two directions run together: an over-cap request is answered before
/// the client has finished writing it.
async fn http(addr: SocketAddr, request: &str) -> io::Result<String> {
    let mut socket = TcpStream::connect(addr).await?;
    let (mut reader, mut writer) = socket.split();
    let mut buf = Vec::new();
    let write = async {
        writer.write_all(request.as_bytes()).await?;
        writer.shutdown().await
    };
    let (_written, read) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(3), write),
        tokio::time::timeout(Duration::from_secs(3), reader.read_to_end(&mut buf)),
    );
    if buf.is_empty() {
        read.map_err(|_| io::Error::other("the callback reply timed out"))??;
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map_or(response, |(_, rest)| rest)
}

/// The listener has entered `serve`: its clock sample is the time before
/// a test moves that clock.
async fn serving() {
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
}

#[tokio::test]
async fn the_redirect_uri_is_the_loopback_callback() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let port = addr.port();
    assert_eq!(
        listener.redirect_uri(),
        format!("http://127.0.0.1:{port}/auth/callback")
    );
    assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    TcpStream::connect(addr).await.expect("127.0.0.1 accepts");
}

#[tokio::test]
async fn one_callback_exchanges_once_and_the_page_hides_the_query() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);
    let response = http(addr, &success(&state)).await.expect("answered");

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("\r\nContent-Type: text/plain; charset=utf-8\r\n"));
    assert!(response.contains("\r\nConnection: close\r\n"));
    assert!(response.contains("\r\nCache-Control: no-store\r\n"));
    assert_eq!(body(&response), super::PAGE_SIGNED_IN);
    assert!(!response.contains(&state), "{response}");
    assert!(!response.contains("code-1"), "{response}");
    assert!(!response.contains("not forwarded"), "{response}");
    assert!(!response.contains("not+forwarded"), "{response}");
    assert_eq!(finish(task).await, CallbackDone::SignedIn);
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
    let forwarded = rig.fake.seen(|seen| {
        let request = &seen.token_requests[0];
        assert!(request.get("scope").is_none());
        assert!(
            request
                .values()
                .all(|value| !value.contains("not forwarded") && !value.contains("not+forwarded"))
        );
        request.get("client_id").cloned()
    });
    assert_eq!(forwarded.as_deref(), Some("oaiapp_1"));
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn a_forged_state_is_ignored_and_the_real_callback_still_exchanges() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);

    let forged = http(
        addr,
        &get("state=forged-state&code=code-1&client_id=oaiapp_1"),
    )
    .await
    .expect("answered");
    assert_eq!(body(&forged), super::PAGE_NOT_THIS);
    assert!(!forged.contains("forged-state"), "{forged}");
    assert!(!forged.contains("code-1"), "{forged}");
    assert!(rig.fake.grants().is_empty());

    let response = http(addr, &success(&state)).await.expect("still listening");
    assert_eq!(body(&response), super::PAGE_SIGNED_IN);
    assert_eq!(finish(task).await, CallbackDone::SignedIn);
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn a_duplicated_parameter_is_refused_and_a_later_callback_exchanges_once() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);

    let duplicated = http(
        addr,
        &get(&format!(
            "state={}&state=again&code=code-1&client_id=oaiapp_1",
            escape(&state)
        )),
    )
    .await
    .expect("answered");
    assert!(duplicated.starts_with("HTTP/1.1 400 "), "{duplicated}");
    assert_eq!(body(&duplicated), super::PAGE_BAD);
    assert!(!duplicated.contains("code-1"), "{duplicated}");
    assert!(rig.fake.grants().is_empty());

    let response = http(addr, &success(&state)).await.expect("still listening");
    assert_eq!(body(&response), super::PAGE_SIGNED_IN);
    assert_eq!(finish(task).await, CallbackDone::SignedIn);
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn a_denial_sends_no_token_request_and_the_page_hides_the_error() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);

    let response = http(
        addr,
        &get(&format!(
            "state={}&error=access_denied&error_description=user+said+no",
            escape(&state)
        )),
    )
    .await
    .expect("answered");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert_eq!(body(&response), super::PAGE_NOT_FINISHED);
    assert!(!response.contains("access_denied"), "{response}");
    assert!(!response.contains("user said no"), "{response}");
    assert!(!response.contains("user+said+no"), "{response}");
    assert!(!response.contains(&state), "{response}");
    assert_eq!(finish(task).await, CallbackDone::Refused);
    assert!(rig.fake.grants().is_empty());
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn a_first_callback_without_a_client_id_reaches_the_broker() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);

    let response = http(addr, &get(&format!("state={}&code=code-1", escape(&state))))
        .await
        .expect("answered");
    assert_eq!(body(&response), super::PAGE_NOT_FINISHED);
    assert!(!response.contains("code-1"), "{response}");
    assert_eq!(finish(task).await, CallbackDone::Refused);
    assert!(rig.fake.grants().is_empty());
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn expiry_closes_the_listener_without_an_exchange() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);
    serving().await;
    rig.advance(601);

    if let Ok(response) = http(addr, &success(&state)).await {
        assert_eq!(body(&response), super::PAGE_ENDED);
        assert!(!response.contains("code-1"), "{response}");
        assert!(!response.contains(&state), "{response}");
    }
    assert_eq!(finish(task).await, CallbackDone::Expired);
    assert!(rig.fake.grants().is_empty());
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn sign_out_closes_the_listener_without_an_exchange() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);
    serving().await;
    rig.plan.sign_out().await.expect("signs out");

    if let Ok(response) = http(addr, &success(&state)).await {
        assert_eq!(body(&response), super::PAGE_ENDED);
        assert!(!response.contains("code-1"), "{response}");
        assert!(!response.contains(&state), "{response}");
    }
    assert_eq!(finish(task).await, CallbackDone::Superseded);
    assert!(rig.fake.grants().is_empty());
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn a_slow_head_does_not_wedge_the_listener() {
    let rig = Rig::new().await;
    let listener = rig
        .listen()
        .await
        .with_read_budget(Duration::from_millis(50));
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);
    serving().await;

    let mut slow = TcpStream::connect(addr).await.expect("connects");
    slow.write_all(b"GET /auth/callback?state=")
        .await
        .expect("writes a partial head");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let response = http(addr, &success(&state))
        .await
        .expect("the listener still answers");
    assert_eq!(body(&response), super::PAGE_SIGNED_IN);
    assert_eq!(finish(task).await, CallbackDone::SignedIn);
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

/// A callback query that already has `state`, plus the rest of its pairs.
fn with_state(enc: &str, rest: &str) -> String {
    get(&format!("state={enc}&{rest}"))
}

/// A request line for the callback path, with an ordinary header block.
fn request_line(enc: &str, verb: &str, version: &str) -> String {
    format!(
        "{verb} /auth/callback?state={enc}&code=code-1&client_id=oaiapp_1 {version}\r\n\
         Host: 127.0.0.1\r\n\r\n"
    )
}

fn oversized_line(enc: &str) -> String {
    let mut line = format!("GET /auth/callback?state={enc}&pad=");
    line.push_str(&"a".repeat(super::MAX_REQUEST_LINE));
    line.push_str(" HTTP/1.1\r\n\r\n");
    line
}

fn oversized_header(enc: &str) -> String {
    let mut head = format!("GET /auth/callback?state={enc}&code=code-1 HTTP/1.1\r\nX-Pad: ");
    head.push_str(&"b".repeat(super::MAX_HEADER_BYTES));
    head.push_str("\r\n\r\n");
    head
}

#[tokio::test]
async fn a_bad_request_does_not_exchange_and_a_good_one_does() {
    let rig = Rig::new().await;
    let listener = rig.listen().await;
    let addr = addr_of(&listener);
    let state = rig.begin(&listener).await;
    let task = spawn_serve(rig.plan.clone(), listener);
    let enc = escape(&state);
    let cases = [
        (
            with_state(&enc, "code=code-1&client_id=oaiapp_1&extra=1"),
            400_u16,
        ),
        (
            with_state(&enc, "code=code-1&code=other&client_id=oaiapp_1"),
            400,
        ),
        (
            with_state(
                &enc,
                "code=code-1&error_description=secret-text&client_id=oaiapp_1",
            ),
            400,
        ),
        (
            with_state(&enc, "code=code-1&error=access_denied&client_id=oaiapp_1"),
            400,
        ),
        (with_state(&enc, "client_id=oaiapp_1"), 400),
        (get("code=code-1&client_id=oaiapp_1"), 400),
        (get("state=&code=code-1&client_id=oaiapp_1"), 400),
        (with_state(&enc, "code=&client_id=oaiapp_1"), 400),
        (with_state(&enc, "code=code-1&client_id="), 400),
        (with_state(&enc, "error="), 400),
        (
            with_state(&enc, "code=code-1&scope=a&scope=b&client_id=oaiapp_1"),
            400,
        ),
        (
            with_state(&enc, "error=x&error_description=a&error_description=b"),
            400,
        ),
        (request_line(&enc, "GET", "HTTP/1.0"), 400),
        ("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_owned(), 404),
        (request_line(&enc, "POST", "HTTP/1.1"), 404),
        (
            "GET /auth/callback HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_owned(),
            400,
        ),
        (format!("GET /other?state={enc} HTTP/1.1\r\n\r\n"), 404),
        (
            format!("GET /auth/callback?state={enc}&code=code-1 HTTP/1.1\n\n"),
            400,
        ),
        (
            format!("GET /auth/callback?a=\0&state={enc} HTTP/1.1\r\n\r\n"),
            400,
        ),
        (oversized_line(&enc), 400),
        (oversized_header(&enc), 400),
    ];

    for (request, status) in cases {
        let response = http(addr, &request).await.expect("answered");
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status} ")),
            "{response}"
        );
        let page = if status == 404 {
            super::PAGE_MISSING
        } else {
            super::PAGE_BAD
        };
        assert_eq!(body(&response), page, "{response}");
        assert!(!response.contains("code-1"), "{response}");
        assert!(!response.contains("secret-text"), "{response}");
        assert!(!response.contains("access_denied"), "{response}");
        assert!(!response.contains(&enc), "{response}");
        assert!(rig.fake.grants().is_empty());
    }

    let response = http(addr, &success(&state)).await.expect("still listening");
    assert_eq!(body(&response), super::PAGE_SIGNED_IN);
    assert_eq!(finish(task).await, CallbackDone::SignedIn);
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}
