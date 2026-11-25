//! The browser callback for one sign-in attempt.
//!
//! [`CallbackListener`] binds `127.0.0.1:0` and answers `GET /auth/callback`
//! until the broker finishes the attempt, the attempt is no longer pending,
//! or ten minutes pass on the same clock the broker uses. A peer off
//! loopback cannot connect, because the socket is bound to `127.0.0.1` and
//! to nothing else.
//!
//! The query accepts the names the real service sends and no others:
//! `state`, `code`, `error`, `error_description`, `client_id`, and `scope`.
//! A duplicated name, an unknown name, a missing or empty `state`, or
//! anything other than exactly one of `code` and `error` is refused, and
//! the attempt stays pending. `error_description` is accepted only beside
//! `error`. `scope` is accepted and dropped; the broker reads the granted
//! scopes from the token response. The page is a fixed sentence. Nothing
//! from the request is written to the page or to a log.
//!
//! Reference: OpenAI's "Registration and sign-in", sections "2. Start
//! authorization" and "3. Handle the callback and exchange the code",
//! <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>.

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use url::form_urlencoded;

use crate::plan::{Callback, ChatGptPlan, Clock, PlanError};

/// How long the listener waits, matching the broker's attempt lifetime.
const ATTEMPT_WINDOW_SECS: u64 = 600;

/// The largest request line, including the query. A sign-in code and a
/// `state` fit with room to spare; anything past this is not a callback.
const MAX_REQUEST_LINE: usize = 8 * 1024;

/// The largest header block after the request line.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// How long one connection may take to send its head, unless a caller
/// shortens it. A sender that dribbles bytes cannot hold the port.
const DEFAULT_READ_BUDGET: Duration = Duration::from_secs(5);

/// How often the loop rechecks the clock and the attempt while no
/// connection is waiting. The deadline is on the broker's clock, which
/// does not wake this task.
const WAKE_EVERY: Duration = Duration::from_millis(200);

const PAGE_SIGNED_IN: &str = "Signed in. You can close this window.\n";
const PAGE_NOT_FINISHED: &str = "Sign-in did not finish. You can close this window.\n";
const PAGE_NOT_THIS: &str = "No sign-in is waiting for this page.\n";
const PAGE_BAD: &str = "This sign-in link was not usable.\n";
const PAGE_ENDED: &str = "This sign-in has ended.\n";
const PAGE_MISSING: &str = "Not found.\n";

/// Why the listener stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackDone {
    /// The broker signed the user in.
    SignedIn,
    /// The broker took the attempt and did not sign the user in.
    Refused,
    /// Ten minutes passed before a usable callback.
    Expired,
    /// The broker no longer has this attempt.
    Superseded,
}

/// Why the listener could not bind or keep its socket.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CallbackError {
    /// `127.0.0.1:0` could not be bound.
    #[error("the sign-in callback listener could not bind")]
    Bind(#[source] io::Error),
    /// The listening socket failed.
    #[error("the sign-in callback listener failed")]
    Io(#[source] io::Error),
}

/// A one-shot loopback listener for one attempt.
#[must_use]
pub struct CallbackListener {
    listener: TcpListener,
    redirect_uri: String,
    clock: Clock,
    read_budget: Duration,
}

impl fmt::Debug for CallbackListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallbackListener")
            .field("redirect_uri", &self.redirect_uri)
            .field("read_budget", &self.read_budget)
            .finish_non_exhaustive()
    }
}

impl CallbackListener {
    /// Binds `127.0.0.1:0`. `clock` should be the clock the broker was
    /// opened with, so this window and the attempt lifetime end together.
    ///
    /// # Errors
    /// [`CallbackError::Bind`] when the socket cannot be bound, or when the
    /// bound address is not `127.0.0.1`.
    pub async fn bind(clock: Clock) -> Result<Self, CallbackError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(CallbackError::Bind)?;
        let addr = listener.local_addr().map_err(CallbackError::Bind)?;
        let SocketAddr::V4(addr) = addr else {
            return Err(CallbackError::Bind(io::Error::other(
                "the callback listener did not bind an IPv4 socket",
            )));
        };
        if *addr.ip() != Ipv4Addr::LOCALHOST {
            return Err(CallbackError::Bind(io::Error::other(
                "the callback listener is not bound to 127.0.0.1",
            )));
        }
        let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", addr.port());
        Ok(Self {
            listener,
            redirect_uri,
            clock,
            read_budget: DEFAULT_READ_BUDGET,
        })
    }

    /// The `redirect_uri` to pass to [`ChatGptPlan::begin_sign_in`].
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Replaces the per-connection read budget. The default is five seconds.
    pub fn with_read_budget(mut self, read_budget: Duration) -> Self {
        self.read_budget = read_budget;
        self
    }

    /// Serves until the attempt ends. Call this after `begin_sign_in`; if
    /// no attempt is pending, it returns [`CallbackDone::Superseded`].
    ///
    /// # Errors
    /// [`CallbackError::Io`] when the listening socket fails.
    pub async fn serve(self, plan: &ChatGptPlan) -> Result<CallbackDone, CallbackError> {
        let started_at = (self.clock)();
        loop {
            if let Some(done) = self.idle(plan, started_at) {
                return Ok(done);
            }
            let accepted = tokio::select! {
                accepted = self.listener.accept() => accepted,
                () = tokio::time::sleep(WAKE_EVERY) => continue,
            };
            let (mut socket, _) = accepted.map_err(CallbackError::Io)?;
            if let Some(done) = self.idle(plan, started_at) {
                let _ = write_page(&mut socket, 200, PAGE_ENDED, self.read_budget).await;
                return Ok(done);
            }
            if let Some(done) = handle(&mut socket, plan, self.read_budget).await {
                return Ok(done);
            }
        }
    }

    /// `Expired` or `Superseded` when the loop should close before it reads
    /// another callback.
    fn idle(&self, plan: &ChatGptPlan, started_at: u64) -> Option<CallbackDone> {
        if (self.clock)().saturating_sub(started_at) > ATTEMPT_WINDOW_SECS {
            return Some(CallbackDone::Expired);
        }
        if !plan.status().sign_in_pending {
            return Some(CallbackDone::Superseded);
        }
        None
    }
}

/// One connection. `Some` closes the listener. `None` keeps it waiting.
async fn handle(
    socket: &mut TcpStream,
    plan: &ChatGptPlan,
    read_budget: Duration,
) -> Option<CallbackDone> {
    let query = match read_query(socket, read_budget).await {
        Ok(query) => query,
        Err(Incoming::NotFound) => {
            let _ = write_page(socket, 404, PAGE_MISSING, read_budget).await;
            return None;
        }
        Err(Incoming::Bad) => {
            let _ = write_page(socket, 400, PAGE_BAD, read_budget).await;
            return None;
        }
    };
    let Ok(callback) = parse_query(&query) else {
        let _ = write_page(socket, 400, PAGE_BAD, read_budget).await;
        return None;
    };
    match plan.complete_sign_in(callback).await {
        Ok(()) => {
            let _ = write_page(socket, 200, PAGE_SIGNED_IN, read_budget).await;
            Some(CallbackDone::SignedIn)
        }
        Err(PlanError::UnknownAttempt) => {
            let _ = write_page(socket, 200, PAGE_NOT_THIS, read_budget).await;
            None
        }
        Err(_) => {
            let _ = write_page(socket, 200, PAGE_NOT_FINISHED, read_budget).await;
            Some(CallbackDone::Refused)
        }
    }
}

/// What a connection was, when it was not a callback query.
enum Incoming {
    /// Not `GET /auth/callback`.
    NotFound,
    /// The head was unreadable, too large, too slow, or not HTTP/1.1.
    Bad,
}

/// The query of one `GET /auth/callback`, or why this connection is not one.
async fn read_query(socket: &mut TcpStream, read_budget: Duration) -> Result<String, Incoming> {
    let head = read_head(socket, read_budget).await?;
    let line_end = head
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .ok_or(Incoming::Bad)?;
    let line = std::str::from_utf8(&head[..line_end]).map_err(|_| Incoming::Bad)?;
    let headers = &head[line_end + 2..];
    if !headers_are_plain(headers) {
        return Err(Incoming::Bad);
    }
    query_from_request_line(line)
}

/// Reads bytes until the header block ends, or the budget or the caps say
/// this is not a callback.
async fn read_head(socket: &mut TcpStream, read_budget: Duration) -> Result<Vec<u8>, Incoming> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    let deadline = tokio::time::Instant::now() + read_budget;
    loop {
        if let Some(end) = header_end(&buf) {
            return Ok(buf[..end].to_vec());
        }
        if over_cap(&buf) {
            return Err(Incoming::Bad);
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(Incoming::Bad);
        }
        let read = tokio::time::timeout(left, socket.read(&mut chunk)).await;
        let count = match read {
            Ok(Ok(0) | Err(_)) | Err(_) => return Err(Incoming::Bad),
            Ok(Ok(count)) => count,
        };
        buf.extend_from_slice(&chunk[..count]);
    }
}

/// The index just past `\r\n\r\n`, when both the request line and the
/// header block are inside their caps. Bytes after the header block are
/// not part of the cap: one read can include the start of a body.
fn header_end(buf: &[u8]) -> Option<usize> {
    let end = buf.windows(4).position(|marker| marker == b"\r\n\r\n")? + 4;
    (!over_cap(buf)).then_some(end)
}

/// Whether `buf` can no longer be a legal head.
fn over_cap(buf: &[u8]) -> bool {
    if buf.contains(&0) || bare_line_feed(buf) {
        return true;
    }
    let Some(line_end) = buf.windows(2).position(|pair| pair == b"\r\n") else {
        return buf.len() > MAX_REQUEST_LINE;
    };
    if line_end > MAX_REQUEST_LINE {
        return true;
    }
    let headers = &buf[line_end + 2..];
    let header_len = headers
        .windows(4)
        .position(|marker| marker == b"\r\n\r\n")
        .map_or(headers.len(), |index| index + 4);
    header_len > MAX_HEADER_BYTES
}

/// A `\n` that is not the second byte of CRLF. A bare line feed would let
/// two readings of the same bytes disagree.
fn bare_line_feed(buf: &[u8]) -> bool {
    buf.iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\n' && (index == 0 || buf[index - 1] != b'\r'))
}

/// Header lines are `name: value` ending in CRLF, with no continuation
/// line. The values are not read.
fn headers_are_plain(headers: &[u8]) -> bool {
    if headers.is_empty() {
        return true;
    }
    let Ok(text) = std::str::from_utf8(headers) else {
        return false;
    };
    text.split("\r\n")
        .filter(|line| !line.is_empty())
        .all(|line| {
            let Some((name, _)) = line.split_once(':') else {
                return false;
            };
            !name.is_empty()
                && !name.starts_with([' ', '\t'])
                && name.bytes().all(|byte| byte.is_ascii_graphic())
        })
}

/// The query when `line` is `GET /auth/callback?query HTTP/1.1`.
fn query_from_request_line(line: &str) -> Result<String, Incoming> {
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Incoming::Bad);
    };
    if version != "HTTP/1.1" {
        return Err(Incoming::Bad);
    }
    if method != "GET" {
        return Err(Incoming::NotFound);
    }
    let Some((path, query)) = target.split_once('?') else {
        return Err(if target == "/auth/callback" {
            Incoming::Bad
        } else {
            Incoming::NotFound
        });
    };
    if path != "/auth/callback" || query.is_empty() || query.contains('#') {
        return Err(if path == "/auth/callback" {
            Incoming::Bad
        } else {
            Incoming::NotFound
        });
    }
    Ok(query.to_owned())
}

/// The broker's [`Callback`] when `query` has exactly the allowed shape.
fn parse_query(query: &str) -> Result<Callback, ()> {
    let mut state = None;
    let mut code = None;
    let mut error = None;
    let mut client_id = None;
    let mut described = false;
    let mut scoped = false;
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        match name.as_ref() {
            "state" => set_once(&mut state, value)?,
            "code" => set_once(&mut code, value)?,
            "error" => set_once(&mut error, value)?,
            "client_id" => set_once(&mut client_id, value)?,
            "error_description" => mark_once(&mut described)?,
            "scope" => mark_once(&mut scoped)?,
            _ => return Err(()),
        }
    }
    let Some(state) = state else {
        return Err(());
    };
    match (code, error) {
        (Some(code), None) if !described => Ok(Callback {
            state,
            code: Some(code),
            error: None,
            client_id,
        }),
        (None, Some(error)) => Ok(Callback {
            state,
            code: None,
            error: Some(error),
            client_id,
        }),
        _ => Err(()),
    }
}

/// Stores `value` when `slot` is empty and `value` is not.
fn set_once(slot: &mut Option<String>, value: Cow<'_, str>) -> Result<(), ()> {
    if slot.is_some() || value.is_empty() {
        return Err(());
    }
    *slot = Some(value.into_owned());
    Ok(())
}

/// Records that an accepted-and-dropped name appeared once.
fn mark_once(seen: &mut bool) -> Result<(), ()> {
    if *seen {
        return Err(());
    }
    *seen = true;
    Ok(())
}

/// Writes one fixed page and closes the write half. The body is one of the
/// constants above; a request byte never reaches it.
async fn write_page(
    socket: &mut TcpStream,
    status: u16,
    body: &str,
    budget: Duration,
) -> io::Result<()> {
    let bytes = format!(
        "\
HTTP/1.1 {status} {reason}\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Length: {length}\r\n\
Connection: close\r\n\
Cache-Control: no-store\r\n\
\r\n\
{body}",
        reason = reason(status),
        length = body.len(),
    );
    tokio::time::timeout(budget, socket.write_all(bytes.as_bytes()))
        .await
        .map_err(|_| io::Error::other("the callback page was not written in time"))??;
    let _ = socket.shutdown().await;
    Ok(())
}

/// The reason phrase for the statuses this listener sends.
const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    }
}

#[cfg(test)]
#[path = "callback-tests.rs"]
mod tests;
