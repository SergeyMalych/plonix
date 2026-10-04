//! Intercept: holding requests (and, if asked, responses) in the proxy so
//! the user can look at each one, edit it, forward it or drop it.
//!
//! Intercept is off when a project opens. While it is on, the proxy asks
//! [`Interceptor::wants`] about every request it is about to send and every
//! response it is about to hand back; a held item waits in the queue until
//! the user forwards it (edited or not) or drops it. An item nobody answers
//! goes on unchanged after the timeout, and turning Intercept off forwards
//! everything still held, so a forgotten queue never leaves a browser
//! hanging.
//!
//! Held items are shown and edited as HTTP/1.1 text (start line, headers, a
//! blank line, the body), whatever protocol the client and server speak. A
//! body is editable when it was read in full (within the recording limit)
//! and is text; otherwise only the start line and headers can be changed and
//! the body goes through as it is.
//!
//! What is never held: tunnels that are not decrypted, the proxy's own pages,
//! WebSocket handshakes and messages, and the engine's own requests (Bench,
//! scans, crawls), which do not pass through the proxy.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::model::{Exchange, Headers, now_ms};
use crate::query::Query;
use crate::settings::{Field, Level, Problem, Section, Values};

/// Which traffic Intercept holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HoldScope {
    /// Only hosts accepted into scope (the default).
    #[default]
    InScope,
    Everything,
}

/// Intercept's options. Saved with the project; whether Intercept is on is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterceptOptions {
    pub hold: HoldScope,
    /// Only traffic matching this search (Traffic's query language) is held.
    pub filter: String,
    /// Hold responses too, not only requests.
    pub responses: bool,
    /// An item nobody answers goes on unchanged after this many seconds.
    pub timeout_s: u64,
}

impl Default for InterceptOptions {
    fn default() -> Self {
        Self { hold: HoldScope::InScope, filter: String::new(), responses: false, timeout_s: DEFAULT_TIMEOUT_S }
    }
}

pub const DEFAULT_TIMEOUT_S: u64 = 300;
pub const MAX_TIMEOUT_S: u64 = 3600;

pub const SETTINGS_SECTION: &str = "intercept";

/// Intercept's options as a Settings section of the project.
pub fn settings_section() -> Section {
    Section::new(SETTINGS_SECTION, "Intercept", Level::Project)
        .describe(
            "What Intercept holds while it is on. Turn it on with the Intercept button in Traffic; it is off whenever the project opens. \
             WebSocket messages, hosts that are never decrypted and Plonix's own requests (Bench, scans, crawls) are never held.",
        )
        .order(15)
        .field(
            Field::choice("hold", "Hold", "in_scope", &[("in_scope", "In-scope hosts only"), ("everything", "Everything")])
                .help("In-scope only lets traffic to other sites through without stopping."),
        )
        .field(
            Field::text("filter", "Only hold traffic matching", "")
                .placeholder("method:POST path:/api")
                .help("A Traffic search. Empty holds everything the setting above allows. For responses, words are matched against the head only."),
        )
        .field(Field::toggle("responses", "Hold responses too", false).help("Edit a response before the browser gets it."))
        .field(
            Field::number("timeout_s", "Forward unanswered items after", DEFAULT_TIMEOUT_S as i64, 1, MAX_TIMEOUT_S as i64)
                .unit("seconds")
                .help("A held item goes on unchanged after this long, so a forgotten queue never leaves the browser hanging."),
        )
        .validator(|v| {
            let filter = v.get("filter").and_then(Value::as_str).unwrap_or("");
            // Named filters (is:) are checked when the engine applies the options.
            match Query::parse_with(filter, &|_| Some(String::new())) {
                Ok(_) => vec![],
                Err(e) => vec![Problem::new("filter", e.to_string())],
            }
        })
}

impl InterceptOptions {
    pub fn from_values(v: &Values) -> Self {
        let d = Self::default();
        Self {
            hold: if v.get("hold").and_then(Value::as_str) == Some("everything") { HoldScope::Everything } else { HoldScope::InScope },
            filter: v.get("filter").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            responses: v.get("responses").and_then(Value::as_bool).unwrap_or(d.responses),
            timeout_s: v.get("timeout_s").and_then(Value::as_u64).unwrap_or(d.timeout_s).clamp(1, MAX_TIMEOUT_S),
        }
    }

    pub fn to_values(&self) -> Values {
        let mut v = Values::new();
        v.insert("hold".into(), Value::from(if self.hold == HoldScope::Everything { "everything" } else { "in_scope" }));
        v.insert("filter".into(), Value::from(self.filter.clone()));
        v.insert("responses".into(), Value::from(self.responses));
        v.insert("timeout_s".into(), Value::from(self.timeout_s));
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HeldKind {
    Request,
    Response,
}

/// One held request or response, as the user sees it.
#[derive(Debug, Clone, Serialize)]
pub struct HeldItem {
    pub id: u64,
    pub kind: HeldKind,
    /// Unix time in milliseconds when it was held.
    pub held_at: i64,
    /// When it goes on unchanged unless the user answers first.
    pub expires_at: i64,
    pub method: String,
    pub url: String,
    pub host: String,
    pub in_scope: bool,
    /// The response status, for a held response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The protocol spoken with the client (requests) or server (responses).
    pub http_version: String,
    /// The item as HTTP/1.1 text.
    pub raw: String,
    /// False when only the start line and headers can be edited.
    pub body_editable: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A request as the user edited it.
#[derive(Debug, Clone, PartialEq)]
pub struct EditedRequest {
    pub method: String,
    /// Path and query.
    pub target: String,
    pub headers: Headers,
    /// `None` when the body was not editable: the original goes through.
    pub body: Option<Vec<u8>>,
}

/// A response as the user edited it.
#[derive(Debug, Clone, PartialEq)]
pub struct EditedResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Request(EditedRequest),
    Response(EditedResponse),
}

/// What the user decided about a held item.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Send it on, edited or as it was.
    Forward(Option<Edit>),
    Drop,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InterceptError {
    #[error("held item {0} not found; it may have gone on already")]
    NotFound(u64),
    #[error("{0}")]
    BadEdit(String),
}

struct Slot {
    item: HeldItem,
    reply: oneshot::Sender<Verdict>,
}

struct Inner {
    on: bool,
    options: InterceptOptions,
    filter: Query,
    queue: Vec<Slot>,
}

/// The held queue and Intercept's switch, one per engine.
pub struct Interceptor {
    inner: Mutex<Inner>,
    /// Changes whenever the queue or the options do, so clients know to look again.
    seq: AtomicU64,
    next_id: AtomicU64,
}

impl Default for Interceptor {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner { on: false, options: InterceptOptions::default(), filter: Query::default(), queue: vec![] }),
            seq: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        }
    }
}

impl Interceptor {
    pub fn is_on(&self) -> bool {
        self.inner.lock().unwrap().on
    }

    pub fn options(&self) -> InterceptOptions {
        self.inner.lock().unwrap().options.clone()
    }

    /// A counter that moves whenever anything here changes.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.seq.fetch_add(1, Ordering::SeqCst);
    }

    /// Replaces the options; `filter` is `options.filter`, already parsed.
    pub fn set_options(&self, options: InterceptOptions, filter: Query) {
        let mut i = self.inner.lock().unwrap();
        i.options = options;
        i.filter = filter;
        drop(i);
        self.bump();
    }

    /// Turns Intercept on or off. Turning it off forwards everything held,
    /// unchanged; returns how many items that released.
    pub fn set_on(&self, on: bool) -> usize {
        let mut i = self.inner.lock().unwrap();
        i.on = on;
        let released = if on { vec![] } else { std::mem::take(&mut i.queue) };
        drop(i);
        let n = released.len();
        for slot in released {
            let _ = slot.reply.send(Verdict::Forward(None));
        }
        self.bump();
        n
    }

    /// Whether this request (or response, with its status and headers) should be held.
    pub fn wants(&self, kind: HeldKind, ex: &Exchange, in_scope: bool) -> bool {
        let i = self.inner.lock().unwrap();
        i.on && (kind == HeldKind::Request || i.options.responses)
            && (i.options.hold == HoldScope::Everything || in_scope)
            && i.filter.matches(ex, in_scope)
    }

    /// The held items, oldest first.
    pub fn queue(&self) -> Vec<HeldItem> {
        self.inner.lock().unwrap().queue.iter().map(|s| s.item.clone()).collect()
    }

    pub fn held(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Holds an item until the user answers or the timeout passes. Returns
    /// the verdict, and whether it went on because time ran out. If the
    /// caller goes away (the client disconnected), the item leaves the queue.
    pub async fn hold(&self, mut item: HeldItem) -> (Verdict, bool) {
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let timeout = {
            let mut i = self.inner.lock().unwrap();
            let timeout = Duration::from_secs(i.options.timeout_s.clamp(1, MAX_TIMEOUT_S));
            item.id = id;
            item.held_at = now_ms();
            item.expires_at = item.held_at + timeout.as_millis() as i64;
            i.queue.push(Slot { item, reply: tx });
            timeout
        };
        self.bump();
        let _leave = Leave { interceptor: self, id };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(verdict)) => (verdict, false),
            Ok(Err(_)) => (Verdict::Forward(None), false),
            Err(_) => (Verdict::Forward(None), true),
        }
    }

    fn take(&self, id: u64) -> Option<Slot> {
        let mut i = self.inner.lock().unwrap();
        let at = i.queue.iter().position(|s| s.item.id == id)?;
        let slot = i.queue.remove(at);
        drop(i);
        self.bump();
        Some(slot)
    }

    /// Sends a held item on. With `raw`, the item goes on as that text; it
    /// is checked first, and a mistake leaves the item held.
    pub fn forward(&self, id: u64, raw: Option<&str>) -> Result<(), InterceptError> {
        let edit = {
            let i = self.inner.lock().unwrap();
            let slot = i.queue.iter().find(|s| s.item.id == id).ok_or(InterceptError::NotFound(id))?;
            match raw {
                Some(raw) if normalize(raw) != normalize(&slot.item.raw) => Some(parse(&slot.item, raw).map_err(InterceptError::BadEdit)?),
                _ => None,
            }
        };
        let slot = self.take(id).ok_or(InterceptError::NotFound(id))?;
        let _ = slot.reply.send(Verdict::Forward(edit));
        Ok(())
    }

    /// Drops a held item: a request is not sent, a response does not reach
    /// the client; either way the client gets an error page.
    pub fn drop_item(&self, id: u64) -> Result<(), InterceptError> {
        let slot = self.take(id).ok_or(InterceptError::NotFound(id))?;
        let _ = slot.reply.send(Verdict::Drop);
        Ok(())
    }

    /// Forwards everything held, unchanged. Returns how many.
    pub fn forward_all(&self) -> usize {
        let released = std::mem::take(&mut self.inner.lock().unwrap().queue);
        let n = released.len();
        for slot in released {
            let _ = slot.reply.send(Verdict::Forward(None));
        }
        self.bump();
        n
    }
}

/// Takes an item out of the queue when its holder stops waiting.
struct Leave<'a> {
    interceptor: &'a Interceptor,
    id: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        self.interceptor.take(self.id);
    }
}

// ---- HTTP/1.1 text ---------------------------------------------------------

/// A request as editable text. `body` is `None` when only the head can be edited.
pub fn request_raw(method: &str, target: &str, headers: &Headers, body: Option<&[u8]>) -> String {
    let mut s = format!("{method} {target} HTTP/1.1\n");
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\n"));
    }
    s.push('\n');
    if let Some(b) = body {
        s.push_str(&String::from_utf8_lossy(b));
    }
    s
}

/// A response as editable text.
pub fn response_raw(status: u16, headers: &Headers, body: Option<&[u8]>) -> String {
    let reason = http::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
    let mut s = format!("HTTP/1.1 {status} {reason}").trim_end().to_string();
    s.push('\n');
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\n"));
    }
    s.push('\n');
    if let Some(b) = body {
        s.push_str(&String::from_utf8_lossy(b));
    }
    s
}

/// Text areas send `\n` where `\r\n` was; compare without the difference.
fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Splits text into its head lines and body.
fn split(raw: &str) -> (Vec<&str>, &str) {
    let raw = raw.trim_start_matches(['\r', '\n']);
    let (head, body) = match (raw.find("\r\n\r\n"), raw.find("\n\n")) {
        (Some(a), Some(b)) if a < b => (&raw[..a], &raw[a + 4..]),
        (_, Some(b)) => (&raw[..b], &raw[b + 2..]),
        (Some(a), None) => (&raw[..a], &raw[a + 4..]),
        (None, None) => (raw, ""),
    };
    (head.lines().map(|l| l.trim_end_matches('\r')).collect(), body)
}

fn parse_headers(lines: &[&str]) -> Result<Headers, String> {
    let mut headers = Headers::new();
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return Err(format!("header lines cannot start with a space: \"{}\"", line.trim()));
        }
        let Some((k, v)) = line.split_once(':') else {
            return Err(format!("\"{line}\" is not a header (Name: value)"));
        };
        let (k, v) = (k.trim(), v.trim());
        if http::HeaderName::from_bytes(k.as_bytes()).is_err() {
            return Err(format!("\"{k}\" is not a valid header name"));
        }
        if http::HeaderValue::from_str(v).is_err() {
            return Err(format!("the value of {k} has characters a header cannot carry"));
        }
        headers.push((k.to_string(), v.to_string()));
    }
    Ok(headers)
}

/// Parses an edited item. The body counts only when it was editable.
fn parse(item: &HeldItem, raw: &str) -> Result<Edit, String> {
    let (lines, body) = split(raw);
    let Some((start, rest)) = lines.split_first() else { return Err("the text is empty".into()) };
    let headers = parse_headers(rest)?;
    let body = item.body_editable.then(|| body.as_bytes().to_vec());
    let parts: Vec<&str> = start.split_whitespace().collect();
    match item.kind {
        HeldKind::Request => {
            let (method, target) = match parts.as_slice() {
                [m, t] | [m, t, _] => (*m, *t),
                _ => return Err(format!("the first line should be METHOD /path HTTP/1.1, not \"{start}\"")),
            };
            if http::Method::from_bytes(method.as_bytes()).is_err() {
                return Err(format!("\"{method}\" is not a valid method"));
            }
            if !target.starts_with('/') || http::uri::PathAndQuery::try_from(target).is_err() {
                return Err(format!("\"{target}\" should be a path such as /api/items?id=1; the destination host cannot be changed here"));
            }
            Ok(Edit::Request(EditedRequest { method: method.to_string(), target: target.to_string(), headers, body }))
        }
        HeldKind::Response => {
            let status = match parts.as_slice() {
                [v, code, ..] if v.starts_with("HTTP/") => code.parse::<u16>().ok(),
                _ => None,
            };
            match status {
                Some(s) if (200..=599).contains(&s) => Ok(Edit::Response(EditedResponse { status: s, headers, body })),
                _ => Err(format!("the first line should be HTTP/1.1 200 OK (a status from 200 to 599), not \"{start}\"")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: HeldKind, raw: &str, body_editable: bool) -> HeldItem {
        HeldItem {
            id: 0,
            kind,
            held_at: 0,
            expires_at: 0,
            method: "POST".into(),
            url: "http://a.test/".into(),
            host: "a.test".into(),
            in_scope: true,
            status: None,
            http_version: "HTTP/1.1".into(),
            raw: raw.into(),
            body_editable,
            note: String::new(),
        }
    }

    #[test]
    fn requests_round_trip_through_text() {
        let headers: Headers = vec![("Host".into(), "a.test".into()), ("Content-Type".into(), "text/plain".into())];
        let raw = request_raw("POST", "/api?x=1", &headers, Some(b"one\ntwo"));
        assert_eq!(raw, "POST /api?x=1 HTTP/1.1\nHost: a.test\nContent-Type: text/plain\n\none\ntwo");
        let it = item(HeldKind::Request, &raw, true);
        let edited = parse(&it, &raw.replace("/api?x=1", "/api?x=2").replace("\n", "\r\n")).unwrap();
        assert_eq!(
            edited,
            Edit::Request(EditedRequest { method: "POST".into(), target: "/api?x=2".into(), headers, body: Some(b"one\r\ntwo".to_vec()) })
        );
        let head_only = item(HeldKind::Request, "GET / HTTP/1.1\n\n", false);
        let Edit::Request(e) = parse(&head_only, "PUT /x\nA: b\n\nignored").unwrap() else { panic!() };
        assert_eq!((e.method.as_str(), e.body), ("PUT", None));
    }

    #[test]
    fn bad_edits_are_explained() {
        let it = item(HeldKind::Request, "", true);
        assert!(parse(&it, "").unwrap_err().contains("empty"));
        assert!(parse(&it, "GET http://other.test/ HTTP/1.1\n\n").unwrap_err().contains("cannot be changed"));
        assert!(parse(&it, "GET / HTTP/1.1\nno colon\n\n").unwrap_err().contains("not a header"));
        assert!(parse(&it, "G(T / HTTP/1.1\n\n").unwrap_err().contains("method"));
        let resp = item(HeldKind::Response, "", true);
        assert!(parse(&resp, "HTTP/1.1 101 Switching\n\n").is_err());
        let Edit::Response(r) = parse(&resp, "HTTP/1.1 418 Teapot\nX: 1\n\nshort").unwrap() else { panic!() };
        assert_eq!((r.status, r.body), (418, Some(b"short".to_vec())));
        assert_eq!(response_raw(204, &vec![], None), "HTTP/1.1 204 No Content\n\n");
    }

    #[tokio::test]
    async fn the_queue_answers_times_out_and_releases() {
        let i = std::sync::Arc::new(Interceptor::default());
        i.set_options(InterceptOptions { timeout_s: 1, ..Default::default() }, Query::default());
        i.set_on(true);
        let ex = Exchange { host: "a.test".into(), method: "GET".into(), ..Default::default() };
        assert!(i.wants(HeldKind::Request, &ex, true));
        assert!(!i.wants(HeldKind::Request, &ex, false), "in-scope only by default");
        assert!(!i.wants(HeldKind::Response, &ex, true), "responses only when asked");

        let raw = request_raw("GET", "/", &vec![], Some(b""));
        let held = { let i = i.clone(); let raw = raw.clone(); tokio::spawn(async move { i.hold(item(HeldKind::Request, &raw, true)).await }) };
        while i.held() == 0 {
            tokio::task::yield_now().await;
        }
        let id = i.queue()[0].id;
        assert!(matches!(i.forward(id, Some("nonsense")), Err(InterceptError::BadEdit(_))));
        assert_eq!(i.held(), 1, "a bad edit leaves the item held");
        i.forward(id, Some(&raw)).unwrap();
        assert_eq!(held.await.unwrap(), (Verdict::Forward(None), false), "unchanged text is no edit");
        assert_eq!(i.forward(id, None), Err(InterceptError::NotFound(id)));

        let (v, timed_out) = i.hold(item(HeldKind::Request, &raw, true)).await;
        assert_eq!((v, timed_out, i.held()), (Verdict::Forward(None), true, 0));

        let held = { let i = i.clone(); let raw = raw.clone(); tokio::spawn(async move { i.hold(item(HeldKind::Request, &raw, true)).await }) };
        while i.held() == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(i.set_on(false), 1);
        assert_eq!(held.await.unwrap().0, Verdict::Forward(None));
        assert!(!i.wants(HeldKind::Request, &ex, true));
    }

    #[test]
    fn options_round_trip_through_settings() {
        let s = settings_section();
        let v = s.check(&serde_json::json!({ "hold": "everything", "filter": "method:POST", "timeout_s": 30 }), &s.resolve(None)).unwrap();
        let o = InterceptOptions::from_values(&v);
        assert_eq!(o, InterceptOptions { hold: HoldScope::Everything, filter: "method:POST".into(), responses: false, timeout_s: 30 });
        assert_eq!(InterceptOptions::from_values(&o.to_values()), o);
        assert_eq!(InterceptOptions::from_values(&s.resolve(None)), InterceptOptions::default());
        assert_eq!(s.check(&serde_json::json!({ "filter": "status:abc" }), &s.resolve(None)).unwrap_err()[0].field, "filter");
    }
}
